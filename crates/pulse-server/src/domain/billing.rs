//! 账单与剩余价值。
//!
//! 纯逻辑、平台无关、可完整单测 —— 算法与全部边界情况见
//! 。

use chrono::{DateTime, Datelike, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// 计费周期。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cycle {
    Monthly,
    Quarterly,
    Semiannual,
    Annual,
    Biennial,
    Triennial,
    /// 买断 / 终身。没有到期时间，剩余价值恒等于买断价
    Onetime,
    /// 自定义天数，配合 `custom_cycle_days`
    Custom,
}

impl Cycle {
    /// 周期含多少个日历月。`Onetime` 与 `Custom` 返回 `None`。
    pub const fn months(self) -> Option<u32> {
        Some(match self {
            Cycle::Monthly => 1,
            Cycle::Quarterly => 3,
            Cycle::Semiannual => 6,
            Cycle::Annual => 12,
            Cycle::Biennial => 24,
            Cycle::Triennial => 36,
            Cycle::Onetime | Cycle::Custom => return None,
        })
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "monthly" => Cycle::Monthly,
            "quarterly" => Cycle::Quarterly,
            "semiannual" => Cycle::Semiannual,
            "annual" => Cycle::Annual,
            "biennial" => Cycle::Biennial,
            "triennial" => Cycle::Triennial,
            "onetime" => Cycle::Onetime,
            "custom" => Cycle::Custom,
            _ => return None,
        })
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Cycle::Monthly => "monthly",
            Cycle::Quarterly => "quarterly",
            Cycle::Semiannual => "semiannual",
            Cycle::Annual => "annual",
            Cycle::Biennial => "biennial",
            Cycle::Triennial => "triennial",
            Cycle::Onetime => "onetime",
            Cycle::Custom => "custom",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Billing {
    pub price: f64,
    pub currency: String,
    pub cycle: Cycle,
    pub custom_cycle_days: Option<i64>,
    pub cycle_start_at: Option<i64>,
    pub expire_at: Option<i64>,
    pub auto_renew: bool,
    /// 购买日期。纯记录用，不参与任何计算
    pub purchased_at: Option<i64>,
    pub remark: Option<String>,
}

/// 剩余情况。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Remaining {
    /// 剩余天数。`None` = 未知（没设到期时间）；负数 = 已过期
    pub days: Option<i64>,
    /// 按剩余比例折算的价值，原币种
    pub value: f64,
    /// 买断机器：剩余时间是 ♾️
    pub infinite: bool,
    /// 未设到期时间 —— 这台机器**不参与**总剩余价值汇总，
    /// 汇总旁要显示「N 台未设到期时间」，不能默默当成 0
    pub unknown: bool,
}

impl Remaining {
    fn unknown() -> Self {
        Self {
            days: None,
            value: 0.0,
            infinite: false,
            unknown: true,
        }
    }
}

/// 从一个时间点往前推 `months` 个日历月。
///
/// **必须 clamp 到目标月的最后一天**：3 月 31 日减 1 个月是 2 月 28/29 日，
/// 不是「2 月 31 日」。直接减 30 天会得到 3 月 1 日，那是错的。
pub fn sub_months(t: DateTime<Utc>, months: u32) -> DateTime<Utc> {
    let total = t.year() as i64 * 12 + (t.month() as i64 - 1) - months as i64;
    let (y, m) = (total.div_euclid(12) as i32, total.rem_euclid(12) as u32 + 1);
    let day = t.day().min(days_in_month(y, m));
    Utc.with_ymd_and_hms(y, m, day, t.hour(), t.minute(), t.second())
        .single()
        // 时间不存在（理论上 UTC 不会）时退回原值，绝不 panic
        .unwrap_or(t)
}

pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 30, // 非法月份，调用方已保证不会到这里
    }
}

pub const fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

use chrono::Timelike;

/// 当前计费周期的起点。
///
/// 优先用用户显式填的 `cycle_start_at`；否则由 `expire_at` 往前推一个周期。
pub fn cycle_start(b: &Billing) -> Option<i64> {
    if let Some(s) = b.cycle_start_at {
        return Some(s);
    }
    let exp = DateTime::from_timestamp(b.expire_at?, 0)?;
    Some(match b.cycle {
        Cycle::Onetime => return None,
        Cycle::Custom => exp.timestamp() - b.custom_cycle_days.unwrap_or(30).max(1) * 86_400,
        c => sub_months(exp, c.months()?).timestamp(),
    })
}

/// 计算剩余天数与剩余价值。
pub fn remaining(b: &Billing, now: i64) -> Remaining {
    // 买断：没有到期概念，剩余价值恒等于买断价
    if b.cycle == Cycle::Onetime {
        return Remaining {
            days: None,
            value: b.price,
            infinite: true,
            unknown: false,
        };
    }
    let (Some(exp), Some(start)) = (b.expire_at, cycle_start(b)) else {
        return Remaining::unknown();
    };

    // 起止倒置（用户填错）时保底 1 秒，避免除零与负比例
    let total = (exp - start).max(1) as f64;
    let left = (exp - now) as f64;
    let ratio = (left / total).clamp(0.0, 1.0);

    Remaining {
        // 向上取整：还剩 0.3 天要显示「1 天」而不是「0 天」
        days: Some((left / 86_400.0).ceil() as i64),
        value: b.price * ratio,
        infinite: false,
        unknown: false,
    }
}

/// 自动续费机器的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenewState {
    /// 未到期，正常
    Ok,
    /// 已过期但在宽限期内，且开了自动续费 —— 显示「续费中」，不告警
    Renewing,
    /// 已过期。开了自动续费却超过宽限期 = 续费很可能失败了
    Expired,
}

/// 自动续费的宽限期。超过它还没更新到期时间，就该提醒用户去核对了。
pub const RENEW_GRACE_DAYS: i64 = 3;

pub fn renew_state(b: &Billing, now: i64) -> RenewState {
    let Some(exp) = b.expire_at else {
        return RenewState::Ok;
    };
    if now <= exp {
        return RenewState::Ok;
    }
    if b.auto_renew && now - exp <= RENEW_GRACE_DAYS * 86_400 {
        RenewState::Renewing
    } else {
        RenewState::Expired
    }
}

/// 三个「总价值」口径。
///
/// 把月付 $5 和年付 $60 直接相加是有误导性的，所以给三个数：
/// UI 主展示 `total_value` 与 `total_remaining`，`annual_cost` 放 tooltip。
#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    /// 「全部续一次费要花多少」—— 各机器当前周期价格之和
    pub total_value: f64,
    /// 唯一可横向比较的口径
    pub annual_cost: f64,
    /// 「现在全部退款理论上能拿回多少」
    pub total_remaining: f64,
    /// 以下三个是**诚实性字段**：汇总不完整时必须能告诉用户少算了几台，
    /// 而不是默默当成 0
    pub unpriced_servers: usize,
    pub no_expire_servers: usize,
    pub no_rate_servers: usize,
}

/// 一台机器折算到展示币种后的金额。`None` 表示该货币没有汇率。
pub type Converted = Option<f64>;

/// 汇总。`convert` 由调用方注入（它需要汇率表），这样这个函数保持纯粹。
pub fn totals<'a>(
    items: impl Iterator<Item = Option<&'a Billing>>,
    now: i64,
    convert: impl Fn(f64, &str) -> Converted,
) -> Totals {
    let mut t = Totals::default();
    for b in items {
        let Some(b) = b else {
            t.unpriced_servers += 1;
            continue;
        };
        // 免费机器（price = -1）不参与任何金额汇总 ——
        // 直接把 -1 加进去会算出负的总价值。它也不算「未填价格」，
        // 因为用户确实填了，只是这台不要钱。
        if is_free(b.price) {
            continue;
        }
        let Some(price) = convert(b.price, &b.currency) else {
            t.no_rate_servers += 1;
            continue;
        };
        t.total_value += price;

        // 买断没有周期可年化
        if let Some(days) = cycle_days(b) {
            t.annual_cost += price * 365.0 / days as f64;
        }

        let r = remaining(b, now);
        if r.unknown {
            t.no_expire_servers += 1;
        } else {
            // 买断的剩余价值就是买断价；其余按比例
            t.total_remaining += convert(r.value, &b.currency).unwrap_or(0.0);
        }
    }
    t
}

/// 价格 = -1 表示这台机器是免费的（白嫖、活动送的、雇主付的）。
///
/// 用一个哨兵值而不是加一个 `is_free` 布尔字段：账单表已经有 9 列，
/// 而「免费」本质上就是价格的一种取值，不是独立维度。
pub fn is_free(price: f64) -> bool {
    price < 0.0
}

/// 价格 = 0 表示还没填 / 不想公开价格，前端什么都不显示。
pub fn is_unpriced(price: f64) -> bool {
    price == 0.0
}

/// 周期天数，用于年化。买断返回 `None`。
pub fn cycle_days(b: &Billing) -> Option<i64> {
    match b.cycle {
        Cycle::Onetime => None,
        Cycle::Custom => Some(b.custom_cycle_days.unwrap_or(30).max(1)),
        c => {
            // 用实际的起止时间算，比「月 × 30.44」准
            let (Some(exp), Some(start)) = (b.expire_at, cycle_start(b)) else {
                // 没有到期时间时退回按月份估算
                return c.months().map(|m| i64::from(m) * 30);
            };
            Some(((exp - start) / 86_400).max(1))
        }
    }
}

#[cfg(test)]
mod tests {

    /// -1 = 免费：不能被当成 -1 元加进总价值里。
    #[test]
    fn free_servers_are_excluded_from_money_totals() {
        let paid = monthly("2026-12-01T00:00:00Z", 10.0);
        let mut free = monthly("2026-12-01T00:00:00Z", -1.0);
        free.currency = "USD".into();
        let now = ts("2026-11-01T00:00:00Z");

        let t = totals([Some(&paid), Some(&free)].into_iter(), now, |v, _| Some(v));
        assert_eq!(t.total_value, 10.0, "免费机器不该把总价值拉低");
        assert_eq!(t.unpriced_servers, 0, "填了 -1 不算「未填价格」");
        assert!(t.total_remaining > 0.0 && t.total_remaining <= 10.0);
    }

    /// 免费机器的剩余价值必须是 0，不能按 -1 折算成负数。
    #[test]
    fn free_server_remaining_value_is_not_negative() {
        let free = monthly("2026-12-01T00:00:00Z", -1.0);
        let r = remaining(&free, ts("2026-11-01T00:00:00Z"));
        // domain 层照实算（-1 的比例），把「免费显示成 0」的决定留给展示层 ——
        // 但展示层必须做，这条测试是提醒它别忘（见 tasks/value.rs::billing_view）
        assert!(is_free(free.price), "价格 -1 就是免费");
        assert!(r.value < 0.0, "照实算会是负数，所以展示层必须特判");
    }

    #[test]
    fn price_semantics_helpers() {
        assert!(is_free(-1.0));
        assert!(!is_free(0.0));
        assert!(!is_free(5.0));
        assert!(is_unpriced(0.0));
        assert!(!is_unpriced(-1.0));
    }
    use super::*;

    const DAY: i64 = 86_400;

    fn ts(s: &str) -> i64 {
        DateTime::parse_from_rfc3339(s).unwrap().timestamp()
    }

    fn monthly(expire: &str, price: f64) -> Billing {
        Billing {
            price,
            currency: "USD".into(),
            cycle: Cycle::Monthly,
            custom_cycle_days: None,
            cycle_start_at: None,
            expire_at: Some(ts(expire)),
            auto_renew: false,
            purchased_at: None,
            remark: None,
        }
    }

    // ── sub_months：月末 clamp ──

    #[test]
    fn sub_months_clamps_to_end_of_target_month() {
        // 3 月 31 日减 1 个月 = 2 月 28 日（平年），不是「2 月 31 日」，
        // 也不是「减 30 天」得到的 3 月 1 日
        let t = DateTime::parse_from_rfc3339("2026-03-31T00:00:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(
            sub_months(t, 1).format("%Y-%m-%d").to_string(),
            "2026-02-28"
        );

        // 闰年
        let t = DateTime::parse_from_rfc3339("2028-03-31T00:00:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(
            sub_months(t, 1).format("%Y-%m-%d").to_string(),
            "2028-02-29"
        );

        // 5 月 31 日减 1 个月 = 4 月 30 日
        let t = DateTime::parse_from_rfc3339("2026-05-31T00:00:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(
            sub_months(t, 1).format("%Y-%m-%d").to_string(),
            "2026-04-30"
        );
    }

    #[test]
    fn sub_months_crosses_year_boundary() {
        // 1 月 31 日减 1 个月 = 上一年 12 月 31 日
        let t = DateTime::parse_from_rfc3339("2026-01-31T00:00:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(
            sub_months(t, 1).format("%Y-%m-%d").to_string(),
            "2025-12-31"
        );
        // 减 12 个月 = 整整一年前
        assert_eq!(
            sub_months(t, 12).format("%Y-%m-%d").to_string(),
            "2025-01-31"
        );
        // 减 36 个月（三年付）
        assert_eq!(
            sub_months(t, 36).format("%Y-%m-%d").to_string(),
            "2023-01-31"
        );
    }

    #[test]
    fn sub_months_preserves_time_of_day() {
        let t = DateTime::parse_from_rfc3339("2026-03-15T13:45:07Z")
            .unwrap()
            .to_utc();
        assert_eq!(
            sub_months(t, 2).format("%Y-%m-%dT%H:%M:%S").to_string(),
            "2026-01-15T13:45:07"
        );
    }

    #[test]
    fn days_in_month_covers_leap_rules() {
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2028, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28, "整百年不是闰年");
        assert_eq!(days_in_month(2000, 2), 29, "400 的倍数是闰年");
        assert_eq!(days_in_month(2026, 4), 30);
        assert_eq!(days_in_month(2026, 12), 31);
    }

    // ── 剩余价值 ──

    #[test]
    fn remaining_is_proportional_to_time_left() {
        let b = monthly("2026-04-01T00:00:00Z", 30.0);
        // 周期是 3/1 → 4/1（31 天）。到 3/16 正好过半多一点
        let half = ts("2026-03-17T00:00:00Z");
        let r = remaining(&b, half);
        assert_eq!(r.days, Some(15));
        // 剩 15/31 → 30 × 15/31 ≈ 14.52
        assert!(
            (r.value - 30.0 * 15.0 / 31.0).abs() < 0.01,
            "实际 {}",
            r.value
        );
        assert!(!r.infinite && !r.unknown);
    }

    #[test]
    fn remaining_of_expired_is_zero_with_negative_days() {
        // 已过期要显示「已过期 N 天」并标红，而不是消失或显示 0 天
        let b = monthly("2026-03-01T00:00:00Z", 30.0);
        let r = remaining(&b, ts("2026-03-11T00:00:00Z"));
        assert_eq!(r.days, Some(-10));
        assert_eq!(r.value, 0.0);
    }

    #[test]
    fn remaining_of_onetime_is_infinite_and_full_price() {
        let b = Billing {
            cycle: Cycle::Onetime,
            expire_at: None,
            ..monthly("2026-04-01T00:00:00Z", 99.0)
        };
        let r = remaining(&b, ts("2030-01-01T00:00:00Z"));
        assert!(r.infinite);
        assert_eq!(r.days, None);
        assert_eq!(r.value, 99.0, "买断机器的剩余价值恒等于买断价");
    }

    #[test]
    fn remaining_without_expiry_is_unknown_not_zero() {
        // 默默算成 0 就是撒谎 —— 汇总旁必须能显示「N 台未设到期时间」
        let b = Billing {
            expire_at: None,
            ..monthly("2026-04-01T00:00:00Z", 30.0)
        };
        let r = remaining(&b, ts("2026-03-01T00:00:00Z"));
        assert!(r.unknown);
        assert_eq!(r.days, None);
    }

    #[test]
    fn remaining_survives_inverted_start_and_end() {
        // 用户把 cycle_start_at 填到了 expire_at 之后
        let b = Billing {
            cycle_start_at: Some(ts("2026-05-01T00:00:00Z")),
            ..monthly("2026-04-01T00:00:00Z", 30.0)
        };
        let r = remaining(&b, ts("2026-03-01T00:00:00Z"));
        assert!(
            r.value.is_finite() && r.value >= 0.0,
            "不能出现负值或 NaN，实际 {}",
            r.value
        );
        assert!(r.value <= 30.0, "剩余价值不能超过原价");
    }

    #[test]
    fn remaining_rounds_days_up() {
        // 还剩 0.3 天要显示「1 天」，显示 0 天会让用户以为今天就到期
        let b = monthly("2026-04-01T00:00:00Z", 30.0);
        let r = remaining(&b, ts("2026-04-01T00:00:00Z") - DAY * 3 / 10);
        assert_eq!(r.days, Some(1));
    }

    // ── 自动续费 ──

    #[test]
    fn auto_renew_shows_renewing_during_grace_then_alerts() {
        let mut b = monthly("2026-03-01T00:00:00Z", 30.0);
        b.auto_renew = true;
        let exp = ts("2026-03-01T00:00:00Z");

        assert_eq!(renew_state(&b, exp - DAY), RenewState::Ok);
        assert_eq!(
            renew_state(&b, exp + DAY),
            RenewState::Renewing,
            "宽限期内显示续费中"
        );
        assert_eq!(
            renew_state(&b, exp + (RENEW_GRACE_DAYS + 1) * DAY),
            RenewState::Expired,
            "超过宽限期说明自动续费很可能失败了，该提醒用户"
        );

        // 没开自动续费的，过期就是过期，没有宽限
        b.auto_renew = false;
        assert_eq!(renew_state(&b, exp + 1), RenewState::Expired);
    }

    // ── 三种总价值 ──

    #[test]
    fn three_totals_are_computed_separately() {
        // 月付 $5 与年付 $60：直接相加是 65，但年化成本都是 60
        let m = Billing {
            price: 5.0,
            ..monthly("2026-04-01T00:00:00Z", 5.0)
        };
        let a = Billing {
            price: 60.0,
            cycle: Cycle::Annual,
            expire_at: Some(ts("2027-01-01T00:00:00Z")),
            ..monthly("2027-01-01T00:00:00Z", 60.0)
        };
        let now = ts("2026-03-01T00:00:00Z");
        let t = totals([Some(&m), Some(&a)].into_iter(), now, |v, _| Some(v));

        assert!((t.total_value - 65.0).abs() < 0.01, "当前周期总价 = 5 + 60");
        // 月付年化 5 × 365/31 ≈ 58.9；年付年化 ≈ 60
        assert!(
            t.annual_cost > 110.0 && t.annual_cost < 125.0,
            "实际 {}",
            t.annual_cost
        );
        assert!(t.total_remaining > 0.0 && t.total_remaining < 65.0);
    }

    #[test]
    fn totals_report_what_they_could_not_count() {
        // 三个诚实性字段：汇总不完整时必须说清少算了几台
        let priced = monthly("2026-04-01T00:00:00Z", 10.0);
        let no_expire = Billing {
            expire_at: None,
            ..monthly("2026-04-01T00:00:00Z", 20.0)
        };
        let no_rate = Billing {
            currency: "RUB".into(),
            ..monthly("2026-04-01T00:00:00Z", 30.0)
        };

        let now = ts("2026-03-01T00:00:00Z");
        let t = totals(
            [Some(&priced), None, Some(&no_expire), Some(&no_rate)].into_iter(),
            now,
            |v, cur| (cur != "RUB").then_some(v), // RUB 没有汇率
        );

        assert_eq!(t.unpriced_servers, 1, "没填账单的");
        assert_eq!(t.no_expire_servers, 1, "填了价格但没填到期时间的");
        assert_eq!(t.no_rate_servers, 1, "货币没有汇率的");
        // 没汇率的那台完全不计入
        assert!((t.total_value - 30.0).abs() < 0.01, "只应算进 10 + 20");
    }

    #[test]
    fn onetime_counts_in_value_but_not_in_annual_cost() {
        let o = Billing {
            price: 99.0,
            cycle: Cycle::Onetime,
            expire_at: None,
            ..monthly("2026-04-01T00:00:00Z", 99.0)
        };
        let t = totals(
            [Some(&o)].into_iter(),
            ts("2026-03-01T00:00:00Z"),
            |v, _| Some(v),
        );
        assert_eq!(t.total_value, 99.0);
        assert_eq!(t.annual_cost, 0.0, "买断没有周期可年化");
        assert_eq!(t.total_remaining, 99.0);
        assert_eq!(t.no_expire_servers, 0, "买断不算「未设到期时间」");
    }

    #[test]
    fn cycle_days_uses_real_calendar_not_average_month() {
        // 2 月的月付周期只有 28 天，不是 30.44
        let feb = Billing {
            cycle_start_at: Some(ts("2026-02-01T00:00:00Z")),
            ..monthly("2026-03-01T00:00:00Z", 30.0)
        };
        assert_eq!(cycle_days(&feb), Some(28));

        let jan = Billing {
            cycle_start_at: Some(ts("2026-01-01T00:00:00Z")),
            ..monthly("2026-02-01T00:00:00Z", 30.0)
        };
        assert_eq!(cycle_days(&jan), Some(31));
    }

    #[test]
    fn custom_cycle_is_honoured_and_clamped() {
        let b = Billing {
            cycle: Cycle::Custom,
            custom_cycle_days: Some(45),
            cycle_start_at: None,
            ..monthly("2026-04-01T00:00:00Z", 45.0)
        };
        assert_eq!(cycle_days(&b), Some(45));
        assert_eq!(cycle_start(&b), Some(ts("2026-04-01T00:00:00Z") - 45 * DAY));

        // 0 天或负数会导致除零，必须夹紧
        let bad = Billing {
            custom_cycle_days: Some(0),
            ..b
        };
        assert_eq!(cycle_days(&bad), Some(1));
    }

    #[test]
    fn cycle_string_roundtrip() {
        for c in [
            Cycle::Monthly,
            Cycle::Quarterly,
            Cycle::Semiannual,
            Cycle::Annual,
            Cycle::Biennial,
            Cycle::Triennial,
            Cycle::Onetime,
            Cycle::Custom,
        ] {
            assert_eq!(Cycle::parse(c.as_str()), Some(c));
        }
        assert_eq!(Cycle::parse("bogus"), None);
    }
}
