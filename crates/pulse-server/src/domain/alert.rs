//! 告警状态机。
//!
//! **UI 状态是即时的，通知是防抖的** —— 两者故意不同步：
//!
//! ```text
//!         条件成立           持续 duration_s          条件消失
//!  ok ──────────────► pending ──────────────► firing ──────────► resolved
//!                        │                      │                   │
//!                     未到时长就恢复          发送通知            发送恢复通知
//!                        └──► ok            冷却 cooldown_s      （可关闭）
//! ```
//!
//! 状态**持久化在数据库**，所以面板重启既不会重复轰炸，
//! 也不会漏掉已经处于 firing 的告警。

use serde::{Deserialize, Serialize};

/// 事件类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    ServerOffline,
    CpuHigh,
    MemHigh,
    DiskHigh,
    TrafficThreshold,
    ExpireSoon,
    PingLossHigh,
    PingLatencyHigh,
}

impl EventKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "server_offline" => EventKind::ServerOffline,
            "cpu_high" => EventKind::CpuHigh,
            "mem_high" => EventKind::MemHigh,
            "disk_high" => EventKind::DiskHigh,
            "traffic_threshold" => EventKind::TrafficThreshold,
            "expire_soon" => EventKind::ExpireSoon,
            "ping_loss_high" => EventKind::PingLossHigh,
            "ping_latency_high" => EventKind::PingLatencyHigh,
            _ => return None,
        })
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            EventKind::ServerOffline => "server_offline",
            EventKind::CpuHigh => "cpu_high",
            EventKind::MemHigh => "mem_high",
            EventKind::DiskHigh => "disk_high",
            EventKind::TrafficThreshold => "traffic_threshold",
            EventKind::ExpireSoon => "expire_soon",
            EventKind::PingLossHigh => "ping_loss_high",
            EventKind::PingLatencyHigh => "ping_latency_high",
        }
    }

    /// 给人看的名字，进通知标题。
    pub const fn label(self) -> &'static str {
        match self {
            EventKind::ServerOffline => "机器离线",
            EventKind::CpuHigh => "CPU 使用率过高",
            EventKind::MemHigh => "内存使用率过高",
            EventKind::DiskHigh => "磁盘使用率过高",
            EventKind::TrafficThreshold => "流量超过阈值",
            EventKind::ExpireSoon => "即将到期",
            EventKind::PingLossHigh => "丢包率过高",
            EventKind::PingLatencyHigh => "延迟过高",
        }
    }

    /// 全部事件类型，供后台下拉与校验。
    pub const ALL: &'static [EventKind] = &[
        EventKind::ServerOffline,
        EventKind::CpuHigh,
        EventKind::MemHigh,
        EventKind::DiskHigh,
        EventKind::TrafficThreshold,
        EventKind::ExpireSoon,
        EventKind::PingLossHigh,
        EventKind::PingLatencyHigh,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertState {
    /// 条件成立了，但还没满足持续时长
    Pending,
    /// 已触发，通知已发出
    Firing,
    /// 条件已恢复
    Resolved,
}

impl AlertState {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => AlertState::Pending,
            "firing" => AlertState::Firing,
            "resolved" => AlertState::Resolved,
            _ => return None,
        })
    }
    pub const fn as_str(self) -> &'static str {
        match self {
            AlertState::Pending => "pending",
            AlertState::Firing => "firing",
            AlertState::Resolved => "resolved",
        }
    }
}

/// 数据库里的一条活跃告警记录。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlertEvent {
    pub state: AlertState,
    /// 条件**首次**成立的时刻。判断是否满足 duration 用它
    pub first_at: i64,
    pub fired_at: Option<i64>,
    pub resolved_at: Option<i64>,
    /// 上次实际发出通知的时刻。冷却用它
    pub last_notified_at: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
pub struct RuleParams {
    /// 条件需持续多久才触发。防的是网络抖动
    pub duration_s: i64,
    /// 触发后多久才允许再次提醒。防的是刷屏
    pub cooldown_s: i64,
    /// 条件恢复时是否发一条恢复通知
    pub notify_resolve: bool,
}

impl Default for RuleParams {
    fn default() -> Self {
        Self {
            duration_s: 90,
            cooldown_s: 3600,
            notify_resolve: true,
        }
    }
}

/// 一次状态推进的产物。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 什么都不做
    Nothing,
    /// 发一条「告警触发」
    NotifyFiring,
    /// 发一条「已恢复」
    NotifyResolved,
}

/// 推进状态机一步。
///
/// `current` 是数据库里的现有记录（`None` = 从没触发过或已清理）。
/// 返回 `(新状态, 要执行的动作)`；新状态为 `None` 表示这条记录可以删掉。
pub fn step(
    current: Option<AlertEvent>,
    condition_met: bool,
    p: &RuleParams,
    now: i64,
) -> (Option<AlertEvent>, Action) {
    match (current, condition_met) {
        // ── 从无到有 ──
        (None, false) => (None, Action::Nothing),
        // 立刻走一次 pending→firing 判定：duration_s = 0 的语义是
        // 「立即触发」，不能让它白等一整轮求值（生产上是 30 秒）
        (None, true) => promote(new_pending(now), p, now),

        // ── pending ──
        // 条件在持续时长内消失：这就是防抖的意义，**不通知**，直接清掉
        (Some(e), false) if e.state == AlertState::Pending => (None, Action::Nothing),
        (Some(e), true) if e.state == AlertState::Pending => promote(e, p, now),

        // ── firing ──
        (Some(e), true) if e.state == AlertState::Firing => {
            let due = e
                .last_notified_at
                .is_none_or(|t| p.cooldown_s > 0 && now - t >= p.cooldown_s);
            if due {
                (
                    Some(AlertEvent {
                        last_notified_at: Some(now),
                        ..e
                    }),
                    Action::NotifyFiring,
                )
            } else {
                (Some(e), Action::Nothing) // 冷却中
            }
        }
        (Some(e), false) if e.state == AlertState::Firing => (
            Some(AlertEvent {
                state: AlertState::Resolved,
                resolved_at: Some(now),
                ..e
            }),
            if p.notify_resolve {
                Action::NotifyResolved
            } else {
                Action::Nothing
            },
        ),

        // ── resolved ──
        // 已恢复的记录留着只是为了让「恢复通知」有据可查，
        // 下一轮就可以清掉
        (Some(_), false) => (None, Action::Nothing),
        // 又坏了：重新从 pending 开始计时，而不是直接沿用旧的 firing ——
        // 否则一个反复抖动的条件会绕过防抖。
        // 但同样要走一次 promote，让 duration_s = 0 的规则保持即时。
        (Some(_), true) => promote(new_pending(now), p, now),
    }
}

fn new_pending(now: i64) -> AlertEvent {
    AlertEvent {
        state: AlertState::Pending,
        first_at: now,
        fired_at: None,
        resolved_at: None,
        last_notified_at: None,
    }
}

/// pending 熬够时长就升级为 firing。
fn promote(e: AlertEvent, p: &RuleParams, now: i64) -> (Option<AlertEvent>, Action) {
    if now - e.first_at >= p.duration_s {
        (
            Some(AlertEvent {
                state: AlertState::Firing,
                fired_at: Some(now),
                last_notified_at: Some(now),
                ..e
            }),
            Action::NotifyFiring,
        )
    } else {
        (Some(e), Action::Nothing) // 还没熬够时长
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: RuleParams = RuleParams {
        duration_s: 90,
        cooldown_s: 3600,
        notify_resolve: true,
    };

    fn advance(
        mut e: Option<AlertEvent>,
        steps: &[(bool, i64)],
        p: &RuleParams,
    ) -> (Option<AlertEvent>, Vec<Action>) {
        let mut acts = Vec::new();
        for (met, now) in steps {
            let (next, a) = step(e, *met, p, *now);
            e = next;
            if a != Action::Nothing {
                acts.push(a);
            }
        }
        (e, acts)
    }

    // ── 五条主路径 ──

    #[test]
    fn path_1_condition_never_met_does_nothing() {
        let (e, acts) = advance(None, &[(false, 0), (false, 100), (false, 200)], &P);
        assert!(e.is_none());
        assert!(acts.is_empty());
    }

    #[test]
    fn path_2_brief_blip_never_notifies() {
        // 这就是防抖存在的意义：网络抖一下就恢复，不该惊动用户
        let (e, acts) = advance(None, &[(true, 0), (true, 30), (false, 60)], &P);
        assert!(e.is_none(), "条件消失后记录应当被清掉");
        assert!(acts.is_empty(), "未满持续时长就恢复，一条通知都不该发");
    }

    #[test]
    fn path_3_sustained_condition_fires_once() {
        let (e, acts) = advance(None, &[(true, 0), (true, 50), (true, 90)], &P);
        assert_eq!(e.unwrap().state, AlertState::Firing);
        assert_eq!(acts, vec![Action::NotifyFiring], "只该发一条");
        assert_eq!(e.unwrap().fired_at, Some(90));
    }

    #[test]
    fn path_4_cooldown_suppresses_repeats_then_allows_one() {
        let mut steps = vec![(true, 0), (true, 90)]; // 触发
                                                     // 冷却期内每 30 秒求值一次，全部应当被抑制
        steps.extend((1..=100).map(|i| (true, 90 + i * 30)));
        let (_, acts) = advance(None, &steps, &P);
        // 90 + 3000 = 3090 < 90+3600；再往后到 3690 才允许第二条
        assert_eq!(acts.len(), 1, "冷却期内不该重复发送，实际 {acts:?}");

        // 跨过冷却之后允许再发一条
        let (_, acts) = advance(None, &[(true, 0), (true, 90), (true, 90 + 3600)], &P);
        assert_eq!(acts, vec![Action::NotifyFiring, Action::NotifyFiring]);
    }

    #[test]
    fn path_5_recovery_sends_resolved_then_record_is_cleared() {
        let (e, acts) = advance(None, &[(true, 0), (true, 90), (false, 200)], &P);
        assert_eq!(e.unwrap().state, AlertState::Resolved);
        assert_eq!(e.unwrap().resolved_at, Some(200));
        assert_eq!(acts, vec![Action::NotifyFiring, Action::NotifyResolved]);

        // 再走一步，记录被清掉
        let (e2, a2) = step(e, false, &P, 260);
        assert!(e2.is_none());
        assert_eq!(a2, Action::Nothing);
    }

    // ── 其余边界 ──

    #[test]
    fn resolve_notification_can_be_disabled() {
        let p = RuleParams {
            notify_resolve: false,
            ..P
        };
        let (e, acts) = advance(None, &[(true, 0), (true, 90), (false, 200)], &p);
        assert_eq!(e.unwrap().state, AlertState::Resolved);
        assert_eq!(acts, vec![Action::NotifyFiring], "关掉之后不该有恢复通知");
    }

    #[test]
    fn flapping_condition_restarts_the_debounce() {
        // 反复抖动的条件不能绕过防抖直接 firing
        let (e, acts) = advance(
            None,
            &[
                (true, 0),
                (true, 90),
                (false, 100),
                (true, 110),
                (true, 150),
            ],
            &P,
        );
        assert_eq!(
            e.unwrap().state,
            AlertState::Pending,
            "重新坏掉要从 pending 重新计时"
        );
        assert_eq!(
            e.unwrap().first_at,
            110,
            "计时起点是重新坏掉的那一刻，不是最初"
        );
        assert_eq!(acts, vec![Action::NotifyFiring, Action::NotifyResolved]);
    }

    #[test]
    fn restart_with_a_firing_record_does_not_re_notify() {
        // **这是状态持久化的意义**：进程重启后从数据库读回 firing，
        // 冷却期内不能再发一条
        let stored = AlertEvent {
            state: AlertState::Firing,
            first_at: 0,
            fired_at: Some(90),
            resolved_at: None,
            last_notified_at: Some(90),
        };
        let (e, a) = step(Some(stored), true, &P, 120);
        assert_eq!(a, Action::Nothing, "重启后不该重复轰炸");
        assert_eq!(e.unwrap().state, AlertState::Firing);
    }

    #[test]
    fn restart_after_cooldown_does_send_a_reminder() {
        // 反过来：冷却已过就该提醒，否则一个持续几天的故障会彻底消音
        let stored = AlertEvent {
            state: AlertState::Firing,
            first_at: 0,
            fired_at: Some(90),
            resolved_at: None,
            last_notified_at: Some(90),
        };
        let (_, a) = step(Some(stored), true, &P, 90 + 3600);
        assert_eq!(a, Action::NotifyFiring);
    }

    #[test]
    fn zero_duration_fires_immediately() {
        // 上线/下线这类事件可以设 duration=0，第一次求值就触发
        let p = RuleParams { duration_s: 0, ..P };
        let (e, acts) = advance(None, &[(true, 100)], &p);
        assert_eq!(e.unwrap().state, AlertState::Firing);
        assert_eq!(acts, vec![Action::NotifyFiring]);
    }

    #[test]
    fn zero_cooldown_does_not_spam_every_evaluation() {
        // cooldown=0 语义是「不重复提醒」，不是「每次求值都发」——
        // 后者在 30 秒一轮的求值下会变成每分钟两条
        let p = RuleParams { cooldown_s: 0, ..P };
        let mut steps = vec![(true, 0), (true, 90)];
        steps.extend((1..=20).map(|i| (true, 90 + i * 30)));
        let (_, acts) = advance(None, &steps, &p);
        assert_eq!(acts.len(), 1, "cooldown=0 应当只发一条，实际 {acts:?}");
    }

    #[test]
    fn clock_going_backwards_does_not_spam() {
        // NTP 校时可能让 now 回退。这时不该把它当成「冷却已过」
        let stored = AlertEvent {
            state: AlertState::Firing,
            first_at: 1000,
            fired_at: Some(1000),
            resolved_at: None,
            last_notified_at: Some(1000),
        };
        let (_, a) = step(Some(stored), true, &P, 500);
        assert_eq!(a, Action::Nothing, "时钟回退不该触发重复通知");
    }

    #[test]
    fn event_kind_roundtrip_and_labels() {
        for k in EventKind::ALL {
            assert_eq!(EventKind::parse(k.as_str()), Some(*k));
            assert!(!k.label().is_empty(), "{k:?} 缺少可读名称");
        }
        assert_eq!(EventKind::parse("bogus"), None);
        // 字符串不能重复，否则规则里两个事件会互相覆盖
        let mut seen = std::collections::HashSet::new();
        for k in EventKind::ALL {
            assert!(seen.insert(k.as_str()), "事件字符串重复: {}", k.as_str());
        }
    }

    #[test]
    fn state_string_roundtrip() {
        for s in [
            AlertState::Pending,
            AlertState::Firing,
            AlertState::Resolved,
        ] {
            assert_eq!(AlertState::parse(s.as_str()), Some(s));
        }
        assert_eq!(AlertState::parse("bogus"), None);
    }
}
