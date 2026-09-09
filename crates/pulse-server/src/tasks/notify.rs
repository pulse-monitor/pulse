//! 通知规则求值。
//!
//! 每 30 秒一轮，**全部条件都在内存里算**（实时层 + 价值缓存），
//! 不查数据库 —— 数据库只用来读写状态机记录。
//!
//! 状态机本身在 `domain::alert`，是纯逻辑并已完整单测；
//! 这里只负责「把条件算出来」和「把通知发出去」。

use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::domain::alert::{self, Action, EventKind};
use crate::notify::{self, template, NotifyMessage};
use crate::state::{AppState, ServerValue};
use crate::store::{ChannelRow, RuleRow, Storage};

/// 单轮求值。
pub async fn evaluate_once(
    state: &AppState,
    store: &Arc<dyn Storage>,
    secret: &[u8],
    panel_url: &str,
    tz: chrono_tz::Tz,
    now: i64,
) -> anyhow::Result<()> {
    let rules: Vec<RuleRow> = store
        .list_rules()
        .await?
        .into_iter()
        .filter(|r| r.enabled && !r.channel_ids.is_empty())
        .collect();
    if rules.is_empty() {
        return Ok(());
    }
    let channels = store.list_channels(secret).await?;
    let values = state.values();
    // **以数据库里的机器列表为准，而不是内存里的连接**。
    //
    // 内存只有「连过 agent 的机器」。面板刚重启时它是空的 ——
    // 而那恰恰是最该发离线告警的时刻，以内存为准的话一条都发不出来。
    // 价值缓存（tasks/value.rs）来自数据库，包含全部机器。
    let live: std::collections::HashMap<_, _> = state
        .list()
        .into_iter()
        .filter_map(|s| state.db_id(&s.id).map(|id| (id, s)))
        .collect();

    for rule in &rules {
        let params = rule.rule_params();
        let scope = rule.scope();

        for val in &values.servers {
            let id = val.server_id;
            // 分组范围需要机器的 group_id；它不在实时层里，
            // 但 PingScope::All 与 Servers 不需要查库 —— 这是最常见的两种
            if !scope.covers(id, None) && !matches!(scope, crate::store::PingScope::Group(_)) {
                continue;
            }
            // 没有实时条目 = 从没连过 / 面板重启后还没重连 —— 视为离线
            let s = live.get(&id).cloned().unwrap_or_else(|| offline_stub(val));
            let s = &s;

            for kind_str in &rule.event_kinds {
                let Some(kind) = EventKind::parse(kind_str) else {
                    continue; // 未知事件类型，忽略而不是让整条规则失效
                };
                let Some(cond) = evaluate(kind, s, Some(val), &rule.params) else {
                    // 条件无法判定（如没装 agent、没填账单）—— 既不触发也不恢复，
                    // 保持现状。把「不知道」当成「正常」会让恢复通知乱发。
                    continue;
                };

                let current = store.get_alert(rule.id, Some(id), kind_str).await?;
                let (next, action) = alert::step(current, cond.met, &params, now);

                match next {
                    Some(e) => {
                        let payload = serde_json::json!({
                            "value": cond.value, "threshold": cond.threshold,
                        });
                        store
                            .upsert_alert(
                                rule.id,
                                Some(id),
                                kind_str,
                                &e,
                                Some(&payload.to_string()),
                            )
                            .await?;
                    }
                    None => store.clear_alert(rule.id, Some(id), kind_str).await?,
                }

                if action != Action::Nothing {
                    dispatch(rule, &channels, s, kind, &cond, action, panel_url, tz, now).await;
                }
            }
        }
    }
    Ok(())
}

/// 为「数据库里有、但内存里没有」的机器造一个离线占位。
///
/// 这样它们照样能被离线规则覆盖，而资源类条件会因为 `online == false`
/// 返回「无法判定」，不会误报。
fn offline_stub(v: &ServerValue) -> crate::state::PublicServer {
    crate::state::PublicServer {
        id: v.uuid.clone(),
        name: v.name.clone(),
        country_code: v.country_code.clone(),
        online: false,
        ..Default::default()
    }
}

/// 一次条件判定的结果。
struct Cond {
    met: bool,
    /// 当前值，已带单位
    value: String,
    /// 阈值，已带单位；没有阈值的事件为空串
    threshold: String,
}

/// 从内存数据判定条件。
///
/// 返回 `None` 表示**无法判定** —— 与「条件不成立」是两回事：
/// 把「不知道」当成「正常」会让恢复通知乱发。
fn evaluate(
    kind: EventKind,
    s: &crate::state::PublicServer,
    v: Option<&ServerValue>,
    params: &serde_json::Value,
) -> Option<Cond> {
    let th = |key: &str, default: f64| params.get(key).and_then(|x| x.as_f64()).unwrap_or(default);

    Some(match kind {
        EventKind::ServerOffline => Cond {
            met: !s.online,
            value: if s.online {
                "在线".into()
            } else {
                "离线".into()
            },
            threshold: String::new(),
        },
        EventKind::CpuHigh => {
            // 离线机器的 CPU 是陈旧值，不该据此告警
            if !s.online {
                return None;
            }
            let t = th("cpu_pct", 90.0);
            Cond {
                met: f64::from(s.cpu_pct) >= t,
                value: format!("{:.1}%", s.cpu_pct),
                threshold: format!("{t:.0}%"),
            }
        }
        EventKind::MemHigh => {
            if !s.online || s.mem.total == 0 {
                return None;
            }
            let t = th("mem_pct", 90.0);
            Cond {
                met: f64::from(s.mem.pct) >= t,
                value: format!("{:.1}%", s.mem.pct),
                threshold: format!("{t:.0}%"),
            }
        }
        EventKind::DiskHigh => {
            if !s.online || s.disk.total == 0 {
                return None;
            }
            let t = th("disk_pct", 90.0);
            Cond {
                met: f64::from(s.disk.pct) >= t,
                value: format!("{:.1}%", s.disk.pct),
                threshold: format!("{t:.0}%"),
            }
        }
        EventKind::PingLossHigh => {
            let l = s.latency.as_ref()?;
            let t = th("loss_pct", 50.0);
            Cond {
                met: l.loss_pct >= t,
                value: format!("{:.1}%", l.loss_pct),
                threshold: format!("{t:.0}%"),
            }
        }
        EventKind::PingLatencyHigh => {
            let l = s.latency.as_ref()?;
            let t = th("rtt_ms", 500.0);
            Cond {
                met: l.rtt_ms >= t,
                value: format!("{:.0} ms", l.rtt_ms),
                threshold: format!("{t:.0} ms"),
            }
        }
        EventKind::TrafficThreshold => {
            let t = v?.traffic.pct?; // 无限流量时 pct 为 None —— 不判定
            let limit = th("traffic_pct", 90.0);
            Cond {
                met: t >= limit,
                value: format!("{t:.1}%"),
                threshold: format!("{limit:.0}%"),
            }
        }
        EventKind::ExpireSoon => {
            let b = v?.billing.as_ref()?;
            let d = b.remain_days?; // 买断或未设到期时间 —— 不判定
            let t = th("days", 7.0) as i64;
            Cond {
                met: d <= t,
                value: if d < 0 {
                    format!("已过期 {} 天", -d)
                } else {
                    format!("剩 {d} 天")
                },
                threshold: format!("{t} 天"),
            }
        }
    })
}

/// 渲染并发到规则绑定的每个渠道。
#[allow(clippy::too_many_arguments)]
async fn dispatch(
    rule: &RuleRow,
    channels: &[ChannelRow],
    s: &crate::state::PublicServer,
    kind: EventKind,
    cond: &Cond,
    action: Action,
    panel_url: &str,
    tz: chrono_tz::Tz,
    now: i64,
) {
    let resolved = action == Action::NotifyResolved;
    let ctx = template::Context {
        server: template::ServerCtx {
            name: s.name.clone(),
            country: s.country_code.clone().unwrap_or_default(),
            os: s.os.clone(),
        },
        event: template::EventCtx {
            kind: kind.as_str().into(),
            label: if resolved {
                format!("{}（已恢复）", kind.label())
            } else {
                kind.label().into()
            },
            value: cond.value.clone(),
            threshold: cond.threshold.clone(),
        },
        time: chrono::DateTime::from_timestamp(now, 0)
            .map(|t| t.with_timezone(&tz).format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default(),
        panel_url: panel_url.to_string(),
    };

    let msg = NotifyMessage {
        title: template::render(
            rule.title_tpl.as_deref().unwrap_or(template::DEFAULT_TITLE),
            &ctx,
        ),
        body: template::render(
            rule.body_tpl.as_deref().unwrap_or(template::DEFAULT_BODY),
            &ctx,
        ),
        resolved,
    };

    for cid in &rule.channel_ids {
        let Some(ch) = channels.iter().find(|c| c.id == *cid && c.enabled) else {
            continue;
        };
        // 一个渠道失败不该影响其他渠道，更不该让求值循环停掉
        match notify::send(&ch.config, &msg).await {
            Ok(()) => info!(
                rule = rule.id,
                channel = ch.id,
                event = kind.as_str(),
                "通知已发送"
            ),
            Err(e) => warn!(rule = rule.id, channel = ch.id, "通知发送失败: {e}"),
        }
    }
    debug!(rule = rule.id, ?action, "告警动作已处理");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{DiskView, LatencyView, MemView, NetView, PublicServer};

    fn srv(online: bool, cpu: f32) -> PublicServer {
        PublicServer {
            id: "u".into(),
            name: "node".into(),
            online,
            os: "Linux".into(),
            kernel: None,
            arch: "x86_64".into(),
            virtualization: None,
            country_code: Some("US".into()),
            latitude: None,
            longitude: None,
            group_id: None,
            buy_url: None,
            review_url: None,
            cpu_model: None,
            cpu_cores: 2,
            uptime_s: 100,
            last_seen: 0,
            agent_version: "0.0.1".into(),
            cpu_pct: cpu,
            load1: None,
            load5: None,
            load15: None,
            mem: MemView {
                total: 1000,
                used: 950,
                pct: 95.0,
                ..Default::default()
            },
            disk: DiskView {
                total: 1000,
                used: 100,
                pct: 10.0,
            },
            net: NetView::default(),
            tcp_conn: None,
            proc_count: None,
            cpu_temp: None,
            gpu: None,
            latency: None,
            latency_spark: Vec::new(),
            loss_spark: Vec::new(),
            capabilities: Default::default(),
        }
    }

    #[test]
    fn offline_condition_is_the_inverse_of_online() {
        let p = serde_json::json!({});
        assert!(
            evaluate(EventKind::ServerOffline, &srv(false, 0.0), None, &p)
                .unwrap()
                .met
        );
        assert!(
            !evaluate(EventKind::ServerOffline, &srv(true, 0.0), None, &p)
                .unwrap()
                .met
        );
    }

    #[test]
    fn offline_machines_do_not_trigger_resource_alerts() {
        // 离线机器的 CPU/内存是陈旧值。据此告警会在机器刚掉线时
        // 同时收到「离线」和「CPU 过高」两条，而后者是假的
        let p = serde_json::json!({});
        assert!(evaluate(EventKind::CpuHigh, &srv(false, 99.0), None, &p).is_none());
        assert!(evaluate(EventKind::MemHigh, &srv(false, 0.0), None, &p).is_none());
        assert!(evaluate(EventKind::DiskHigh, &srv(false, 0.0), None, &p).is_none());
    }

    #[test]
    fn thresholds_come_from_rule_params_with_defaults() {
        let s = srv(true, 85.0);
        // 默认 90%：85 不触发
        assert!(
            !evaluate(EventKind::CpuHigh, &s, None, &serde_json::json!({}))
                .unwrap()
                .met
        );
        // 规则里调成 80%：触发
        let p = serde_json::json!({ "cpu_pct": 80 });
        let c = evaluate(EventKind::CpuHigh, &s, None, &p).unwrap();
        assert!(c.met);
        assert_eq!(c.value, "85.0%");
        assert_eq!(c.threshold, "80%");
    }

    #[test]
    fn undecidable_conditions_return_none_not_false() {
        // 「不知道」与「正常」是两回事：把前者当后者会让恢复通知乱发
        let p = serde_json::json!({});
        let s = srv(true, 0.0);
        // 没有延迟数据
        assert!(evaluate(EventKind::PingLossHigh, &s, None, &p).is_none());
        // 没有价值缓存 → 流量与到期都判定不了
        assert!(evaluate(EventKind::TrafficThreshold, &s, None, &p).is_none());
        assert!(evaluate(EventKind::ExpireSoon, &s, None, &p).is_none());
    }

    #[test]
    fn zero_total_resources_are_undecidable() {
        // 采集失败时 total 为 0，算出来的百分比没有意义
        let mut s = srv(true, 0.0);
        s.mem.total = 0;
        assert!(evaluate(EventKind::MemHigh, &s, None, &serde_json::json!({})).is_none());
        s.disk.total = 0;
        assert!(evaluate(EventKind::DiskHigh, &s, None, &serde_json::json!({})).is_none());
    }

    #[test]
    fn latency_conditions_use_the_latest_sample() {
        let mut s = srv(true, 0.0);
        s.latency = Some(LatencyView {
            task_id: 1,
            rtt_ms: 800.0,
            loss_pct: 60.0,
        });
        let p = serde_json::json!({});
        let loss = evaluate(EventKind::PingLossHigh, &s, None, &p).unwrap();
        assert!(loss.met && loss.value == "60.0%");
        let rtt = evaluate(EventKind::PingLatencyHigh, &s, None, &p).unwrap();
        assert!(rtt.met && rtt.value == "800 ms");
    }
}
