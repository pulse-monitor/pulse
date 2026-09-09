//! 流量统计。
//!
//! 纯逻辑，可完整单测。算法与边界

use chrono::{DateTime, Datelike, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use super::billing::days_in_month;

/// 阈值的统计口径（R5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalcMode {
    /// 上行 + 下行
    #[default]
    Sum,
    Max,
    Min,
    /// 只算上行
    Upload,
    /// 只算下行
    Download,
}

impl CalcMode {
    pub fn parse(s: &str) -> Self {
        match s {
            "max" => CalcMode::Max,
            "min" => CalcMode::Min,
            "upload" => CalcMode::Upload,
            "download" => CalcMode::Download,
            // 未知值回落 sum：数据被改坏时给一个保守可解释的结果，
            // 而不是让整台机器的流量统计消失
            _ => CalcMode::Sum,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            CalcMode::Sum => "sum",
            CalcMode::Max => "max",
            CalcMode::Min => "min",
            CalcMode::Upload => "upload",
            CalcMode::Download => "download",
        }
    }
}

/// 按口径算出「已用量」。
///
/// 数据库里**只存 in/out 两个方向**，所以改口径之后历史周期也能按新口径重算。
pub fn used(in_bytes: u64, out_bytes: u64, mode: CalcMode) -> u64 {
    match mode {
        CalcMode::Sum => in_bytes.saturating_add(out_bytes),
        CalcMode::Max => in_bytes.max(out_bytes),
        CalcMode::Min => in_bytes.min(out_bytes),
        CalcMode::Upload => out_bytes,
        CalcMode::Download => in_bytes,
    }
}

/// 由网卡累计计数算出本次的增量。
///
/// agent 上报的是**累计值**而不是增量，正是为了让这里能正确处理三种情况
/// ：
///
/// | 情况 | 表现 | 处理 |
/// |---|---|---|
/// | 正常 | `cur >= last` | 差值 |
/// | 机器重启 / agent 重装 | 计数器归零，`cur < last` | 只计 `cur` |
/// | 首次观测 | `last` 为 None | 计 0，只建基线 |
pub fn delta(cur: u64, last: Option<u64>) -> u64 {
    match last {
        None => 0,
        Some(l) if cur >= l => cur - l,
        // 计数器归零：把 cur 全算上，比丢掉整段更接近真相
        Some(_) => cur,
    }
}

/// 当前统计周期的起点。
///
/// `reset_day` ∈ [1,31]，31 表示「当月最后一天」。
/// 时区用面板时区而不是机器本地时区 —— 账单周期是「你和商家的约定」，
/// 用统一时区可解释；机器时区五花八门会让同一天的重置发生在不同时刻。
pub fn period_start(now: DateTime<Utc>, reset_day: u32, tz: Tz) -> DateTime<Utc> {
    let local = now.with_timezone(&tz);
    let candidate = day_in_month(local.year(), local.month(), reset_day, tz);
    if let Some(c) = candidate {
        if local >= c {
            return c.to_utc();
        }
    }
    // 还没到本月的重置日 → 用上个月的
    let (y, m) = prev_month(local.year(), local.month());
    day_in_month(y, m, reset_day, tz)
        .map(|d| d.to_utc())
        // 极端情况（夏令时导致当天 00:00 不存在且回落也失败）：
        // 退回「30 天前」，绝不 panic
        .unwrap_or(now - chrono::Duration::days(30))
}

/// 下一个周期起点。
pub fn next_period_start(current: DateTime<Utc>, reset_day: u32, tz: Tz) -> DateTime<Utc> {
    let local = current.with_timezone(&tz);
    let (y, m) = next_month(local.year(), local.month());
    day_in_month(y, m, reset_day, tz)
        .map(|d| d.to_utc())
        .unwrap_or(current + chrono::Duration::days(30))
}

/// 某年某月的第 `day` 天 00:00（本地时区）。`day` 超过当月天数时取最后一天。
fn day_in_month(year: i32, month: u32, day: u32, tz: Tz) -> Option<DateTime<Tz>> {
    let d = day.clamp(1, 31).min(days_in_month(year, month));
    // 夏令时可能让当天 00:00 不存在；往后找到第一个存在的小时
    for hour in 0..6 {
        if let Some(t) = tz.with_ymd_and_hms(year, month, d, hour, 0, 0).single() {
            return Some(t);
        }
    }
    None
}

fn prev_month(y: i32, m: u32) -> (i32, u32) {
    if m == 1 {
        (y - 1, 12)
    } else {
        (y, m - 1)
    }
}

fn next_month(y: i32, m: u32) -> (i32, u32) {
    if m == 12 {
        (y + 1, 1)
    } else {
        (y, m + 1)
    }
}

/// 面板停机后要补做的周期归档。
///
/// 停机三天跨过了两个周期时，必须把中间每一个都归档，
/// 而不是只做最后一个 —— 否则中间那些周期的数据就丢了。
/// 返回按时间升序的周期边界列表。
pub fn periods_to_settle(
    stored_start: DateTime<Utc>,
    now: DateTime<Utc>,
    reset_day: u32,
    tz: Tz,
) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
    let current = period_start(now, reset_day, tz);
    let mut out = Vec::new();
    let mut s = stored_start;
    // 失控防护：最多补 200 个周期（约 16 年），异常数据不能变成死循环
    for _ in 0..200 {
        if s >= current {
            break;
        }
        let e = next_period_start(s, reset_day, tz);
        out.push((s, e));
        if e <= s {
            break; // 没有推进，说明日期计算异常，止损
        }
        s = e;
    }
    out
}

/// 跨过了哪些告警档位。
///
/// `already` 是本周期已经报过的档位，避免重复轰炸；周期重置时清空。
pub fn crossed_thresholds(used: u64, limit: Option<u64>, pcts: &[u8], already: &[u8]) -> Vec<u8> {
    let Some(limit) = limit.filter(|l| *l > 0) else {
        return Vec::new(); // 无限流量不告警
    };
    let ratio = used as f64 * 100.0 / limit as f64;
    let mut out: Vec<u8> = pcts
        .iter()
        .copied()
        .filter(|p| ratio >= f64::from(*p) && !already.contains(p))
        .collect();
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: u64 = 1 << 30;
    const SHANGHAI: Tz = chrono_tz::Asia::Shanghai;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().to_utc()
    }
    fn ymd(d: DateTime<Utc>, tz: Tz) -> String {
        d.with_timezone(&tz).format("%Y-%m-%d %H:%M").to_string()
    }

    // ── delta ──

    #[test]
    fn delta_handles_the_three_cases() {
        assert_eq!(delta(1000, Some(400)), 600, "正常");
        assert_eq!(
            delta(5, Some(1_000_000)),
            5,
            "计数器归零（重启/重装）：只计当前值"
        );
        assert_eq!(delta(1000, None), 0, "首次观测只建基线，不计流量");
        assert_eq!(delta(0, Some(0)), 0);
    }

    #[test]
    fn delta_never_overflows() {
        assert_eq!(delta(u64::MAX, Some(0)), u64::MAX);
        assert_eq!(delta(0, Some(u64::MAX)), 0, "归零场景");
    }

    // ── 五种口径 ──

    #[test]
    fn all_five_calc_modes() {
        let (i, o) = (3 * G, 7 * G);
        assert_eq!(used(i, o, CalcMode::Sum), 10 * G);
        assert_eq!(used(i, o, CalcMode::Max), 7 * G);
        assert_eq!(used(i, o, CalcMode::Min), 3 * G);
        assert_eq!(used(i, o, CalcMode::Upload), 7 * G, "upload = 出站");
        assert_eq!(used(i, o, CalcMode::Download), 3 * G, "download = 入站");
    }

    #[test]
    fn calc_mode_sum_saturates_instead_of_wrapping() {
        assert_eq!(used(u64::MAX, u64::MAX, CalcMode::Sum), u64::MAX);
    }

    #[test]
    fn calc_mode_parse_falls_back_to_sum() {
        // 数据被改坏时给保守可解释的结果，而不是让统计消失
        assert_eq!(CalcMode::parse("garbage"), CalcMode::Sum);
        for m in [
            CalcMode::Sum,
            CalcMode::Max,
            CalcMode::Min,
            CalcMode::Upload,
            CalcMode::Download,
        ] {
            assert_eq!(CalcMode::parse(m.as_str()), m);
        }
    }

    // ── 月重置日 ──

    #[test]
    fn period_start_before_and_after_reset_day() {
        // 重置日 15：3 月 10 日属于「2 月 15 → 3 月 15」这个周期
        assert_eq!(
            ymd(
                period_start(t("2026-03-10T12:00:00+08:00"), 15, SHANGHAI),
                SHANGHAI
            ),
            "2026-02-15 00:00"
        );
        // 3 月 20 日属于「3 月 15 → 4 月 15」
        assert_eq!(
            ymd(
                period_start(t("2026-03-20T12:00:00+08:00"), 15, SHANGHAI),
                SHANGHAI
            ),
            "2026-03-15 00:00"
        );
        // 正好是重置日当天 00:00，算新周期
        assert_eq!(
            ymd(
                period_start(t("2026-03-15T00:00:00+08:00"), 15, SHANGHAI),
                SHANGHAI
            ),
            "2026-03-15 00:00"
        );
    }

    #[test]
    fn reset_day_31_means_last_day_of_month() {
        // 2 月只有 28 天，reset_day=31 应当落在 2 月 28 日
        assert_eq!(
            ymd(
                period_start(t("2026-03-05T00:00:00+08:00"), 31, SHANGHAI),
                SHANGHAI
            ),
            "2026-02-28 00:00"
        );
        // 闰年 2 月 29 日
        assert_eq!(
            ymd(
                period_start(t("2028-03-05T00:00:00+08:00"), 31, SHANGHAI),
                SHANGHAI
            ),
            "2028-02-29 00:00"
        );
        // 4 月只有 30 天
        assert_eq!(
            ymd(
                period_start(t("2026-05-05T00:00:00+08:00"), 31, SHANGHAI),
                SHANGHAI
            ),
            "2026-04-30 00:00"
        );
        // 3 月有 31 天，正常落在 31 日
        assert_eq!(
            ymd(
                period_start(t("2026-04-05T00:00:00+08:00"), 31, SHANGHAI),
                SHANGHAI
            ),
            "2026-03-31 00:00"
        );
    }

    #[test]
    fn reset_day_29_in_a_common_year_february() {
        // 平年 2 月没有 29 日，应当落到 28 日
        assert_eq!(
            ymd(
                period_start(t("2026-03-05T00:00:00+08:00"), 29, SHANGHAI),
                SHANGHAI
            ),
            "2026-02-28 00:00"
        );
        // 闰年就有
        assert_eq!(
            ymd(
                period_start(t("2028-03-05T00:00:00+08:00"), 29, SHANGHAI),
                SHANGHAI
            ),
            "2028-02-29 00:00"
        );
    }

    #[test]
    fn period_start_crosses_year_boundary() {
        // 1 月 5 日、重置日 15 → 上一年 12 月 15 日
        assert_eq!(
            ymd(
                period_start(t("2026-01-05T00:00:00+08:00"), 15, SHANGHAI),
                SHANGHAI
            ),
            "2025-12-15 00:00"
        );
    }

    #[test]
    fn period_start_respects_timezone() {
        // 同一个 UTC 时刻，在不同时区属于不同的日期，因而可能属于不同周期。
        // UTC 时间 2026-03-14 20:00 = 上海 3/15 04:00 = 纽约 3/14 16:00
        let now = t("2026-03-14T20:00:00Z");
        assert_eq!(
            ymd(period_start(now, 15, SHANGHAI), SHANGHAI),
            "2026-03-15 00:00"
        );
        let ny = chrono_tz::America::New_York;
        assert_eq!(ymd(period_start(now, 15, ny), ny), "2026-02-15 00:00");
    }

    #[test]
    fn period_start_survives_dst_transitions() {
        // 一些时区在夏令时切换日没有 00:00（时钟直接跳过）。
        // 圣地亚哥（智利）9 月的某天会跳过午夜那一小时。
        let santiago = chrono_tz::America::Santiago;
        for day in 1..=28u32 {
            let now = Utc.with_ymd_and_hms(2026, 9, 28, 12, 0, 0).unwrap();
            let p = period_start(now, day, santiago);
            assert!(p < now, "day={day} 的周期起点必须在当前时刻之前");
        }
    }

    // ── 停机补做 ──

    #[test]
    fn panel_downtime_settles_every_missed_period() {
        // 停机三个月后启动：中间每一个周期都要归档，不能只做最后一个
        let stored = t("2026-01-01T00:00:00+08:00");
        let now = t("2026-04-10T00:00:00+08:00");
        let ps = periods_to_settle(stored, now, 1, SHANGHAI);

        assert_eq!(ps.len(), 3, "1月、2月、3月三个周期都要归档");
        assert_eq!(ymd(ps[0].0, SHANGHAI), "2026-01-01 00:00");
        assert_eq!(ymd(ps[0].1, SHANGHAI), "2026-02-01 00:00");
        assert_eq!(ymd(ps[2].0, SHANGHAI), "2026-03-01 00:00");
        assert_eq!(ymd(ps[2].1, SHANGHAI), "2026-04-01 00:00");
    }

    #[test]
    fn no_settlement_needed_within_the_same_period() {
        let stored = t("2026-03-01T00:00:00+08:00");
        let now = t("2026-03-20T00:00:00+08:00");
        assert!(periods_to_settle(stored, now, 1, SHANGHAI).is_empty());
    }

    #[test]
    fn settlement_is_bounded_against_corrupt_data() {
        // period_start 被改成了 1970 年：不能变成死循环
        let stored = t("1970-01-01T00:00:00Z");
        let now = t("2026-04-10T00:00:00+08:00");
        let ps = periods_to_settle(stored, now, 1, SHANGHAI);
        assert!(ps.len() <= 200, "必须有上限，实际 {}", ps.len());
    }

    // ── 阈值 ──

    #[test]
    fn thresholds_fire_once_each_per_period() {
        let pcts = [80u8, 95, 100];
        // 85% → 只跨过 80
        assert_eq!(
            crossed_thresholds(85 * G, Some(100 * G), &pcts, &[]),
            vec![80]
        );
        // 已经报过 80，再到 96% 只报 95
        assert_eq!(
            crossed_thresholds(96 * G, Some(100 * G), &pcts, &[80]),
            vec![95]
        );
        // 超了 100%，且前面都报过
        assert_eq!(
            crossed_thresholds(120 * G, Some(100 * G), &pcts, &[80, 95]),
            vec![100]
        );
        // 全报过了就不再报
        assert!(crossed_thresholds(120 * G, Some(100 * G), &pcts, &[80, 95, 100]).is_empty());
    }

    #[test]
    fn a_sudden_jump_reports_all_crossed_levels_at_once() {
        // 面板停机期间流量从 10% 涨到 120%：三个档位应当一次全报出来，
        // 而不是只报最高的那个（用户需要知道它是一路冲过去的）
        assert_eq!(
            crossed_thresholds(120 * G, Some(100 * G), &[80, 95, 100], &[]),
            vec![80, 95, 100]
        );
    }

    #[test]
    fn unlimited_traffic_never_alerts() {
        assert!(crossed_thresholds(u64::MAX, None, &[80, 95, 100], &[]).is_empty());
        // limit=0 也当成无限，避免除零
        assert!(crossed_thresholds(1, Some(0), &[80, 95, 100], &[]).is_empty());
    }
}
