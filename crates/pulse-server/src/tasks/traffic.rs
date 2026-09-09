//! 流量结算任务。
//!
//! 每分钟一轮：把 agent 上报的**网卡累计计数**转成本周期的增量，
//! 跨周期时归档并重置。算法

use std::sync::Arc;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use tracing::{debug, info, warn};

use crate::domain::traffic;
use crate::state::AppState;
use crate::store::{Storage, TrafficRow};

/// 网卡集合变化时要重置基线。
///
/// 用户改了网卡过滤规则之后，累计计数的口径就变了 —— 不重置的话
/// 会产生一次巨大的假跳变（比如从只统计 eth0 变成统计全部网卡）。
fn ifaces_changed(stored: &str, current: &[String]) -> bool {
    let mut cur = current.to_vec();
    cur.sort();
    stored != cur.join(",")
}

pub async fn settle_once(
    state: &AppState,
    store: &Arc<dyn Storage>,
    panel_tz: Tz,
    now: i64,
) -> anyhow::Result<()> {
    let now_dt = DateTime::from_timestamp(now, 0).unwrap_or_else(Utc::now);

    for (id, rx, tx, ifaces) in state.net_counters() {
        let cfg = store.get_traffic_config(id).await?;
        let tz = cfg
            .timezone
            .as_deref()
            .and_then(|s| s.parse::<Tz>().ok())
            .unwrap_or(panel_tz);

        let mut row = match store.get_traffic(id).await? {
            Some(r) => r,
            None => TrafficRow {
                server_id: id,
                period_start: traffic::period_start(now_dt, cfg.reset_day, tz).timestamp(),
                ..Default::default()
            },
        };

        // ── 跨周期：把中间**每一个**周期都归档，不是只做最后一个 ──
        let stored_start = DateTime::from_timestamp(row.period_start, 0).unwrap_or(now_dt);
        let periods = traffic::periods_to_settle(stored_start, now_dt, cfg.reset_day, tz);
        if !periods.is_empty() {
            // 当期累计量归到它所属的那个周期；中间被跳过的周期归档成 0 ——
            // 面板停机期间本来就没采到数据
            let (first_s, first_e) = periods[0];
            store
                .archive_traffic(
                    id,
                    first_s.timestamp(),
                    first_e.timestamp(),
                    row.in_bytes,
                    row.out_bytes,
                )
                .await?;
            for (s, e) in periods.iter().skip(1) {
                store
                    .archive_traffic(id, s.timestamp(), e.timestamp(), 0, 0)
                    .await?;
            }
            info!(id, periods = periods.len(), "流量周期归档");

            row.period_start = periods.last().map(|(_, e)| e.timestamp()).unwrap_or(now);
            row.in_bytes = 0;
            row.out_bytes = 0;
            row.alerted_pct.clear(); // 新周期重新计算告警档位
        }

        // ── 增量 ──
        let mut sorted = ifaces.clone();
        sorted.sort();
        let iface_key = sorted.join(",");
        let stored_key = store
            .get_setting(&format!("traffic.ifaces.{id}"))
            .await?
            .unwrap_or_default();

        if !stored_key.is_empty() && ifaces_changed(&stored_key, &ifaces) {
            warn!(id, "网卡集合已变化，重置流量基线（避免一次假跳变）");
            row.last_raw_in = None;
            row.last_raw_out = None;
        }
        if stored_key != iface_key {
            store
                .set_setting(&format!("traffic.ifaces.{id}"), &iface_key, now)
                .await?;
        }

        let d_in = traffic::delta(rx, row.last_raw_in.map(|v| v as u64));
        let d_out = traffic::delta(tx, row.last_raw_out.map(|v| v as u64));
        row.in_bytes = row.in_bytes.saturating_add(d_in as i64);
        row.out_bytes = row.out_bytes.saturating_add(d_out as i64);
        row.last_raw_in = Some(rx as i64);
        row.last_raw_out = Some(tx as i64);

        // ── 阈值 ──
        let mode = traffic::CalcMode::parse(&cfg.calc_mode);
        let used = traffic::used(row.in_bytes as u64, row.out_bytes as u64, mode);
        let crossed = traffic::crossed_thresholds(
            used,
            cfg.limit_bytes.map(|v| v as u64),
            &cfg.alert_pct,
            &row.alerted_pct,
        );
        if !crossed.is_empty() {
            // M6 会把这里接到通知渠道；现在先记日志并标记，
            // 「每档每周期只报一次」的语义已经成立
            warn!(id, ?crossed, used, limit = ?cfg.limit_bytes, "流量跨过告警档位");
            row.alerted_pct.extend(crossed);
            row.alerted_pct.sort_unstable();
            row.alerted_pct.dedup();
        }

        store.upsert_traffic(&row, now).await?;
        debug!(id, d_in, d_out, used, "流量结算");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iface_change_detection() {
        // 顺序不同不算变化 —— agent 每次上报的网卡顺序可能不一样
        assert!(!ifaces_changed(
            "eth0,eth1",
            &["eth1".into(), "eth0".into()]
        ));
        assert!(!ifaces_changed("eth0", &["eth0".into()]));
        // 真的变了才算
        assert!(ifaces_changed("eth0", &["eth0".into(), "eth1".into()]));
        assert!(ifaces_changed("eth0,eth1", &["eth0".into()]));
        assert!(ifaces_changed("eth0", &[]));
    }
}
