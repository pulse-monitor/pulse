//! 汇率换算与降级。
//!
//! 源是 `rate.jinqians.com`（自建 Frankfurter）。**已实测的限制**：
//!
//! - 基于 ECB，只覆盖 **30 种**货币，且**每个工作日更新一次**（约 CET 16:00）
//! - VPS 场景常见但**不覆盖**的：`RUB`（俄区商家）、`TWD`、`VND`、`UAH`、
//!   `ARS`、`AED`。这些必须走人工汇率
//! - 覆盖的包括：USD EUR CNY GBP HKD JPY SGD AUD CAD KRW INR TRY BRL 等
//!
//! 所以降级不是可选的 —— 见 [`Level`]。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// 超过这个时长没更新就算「陈旧」，UI 要标注。
///
/// 源每个工作日更新一次，周末不变，所以 48 小时才算陈旧 ——
/// 定得太短会让整个周末都在报警。
pub const STALE_AFTER_S: i64 = 48 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// 自动拉取
    Frankfurter,
    /// 人工填写。**优先级高于自动拉取** —— 用于源不支持的货币
    Manual,
}

#[derive(Debug, Clone, Copy)]
pub struct Entry {
    /// 1 USD = `rate` 单位的该货币
    pub rate: f64,
    pub source: Source,
}

/// 换算结果所处的降级层级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// L0：汇率新鲜
    Fresh,
    /// L1：拉取失败，用的是缓存的旧值。UI 要标「汇率更新于 X 前」
    Stale,
    /// L2：源不支持这个货币，用的是人工填的值。UI 标「人工汇率」
    Manual,
    /// L3：既无缓存也无人工值。该机器**不参与汇总**，
    /// 汇总旁要显示「N 台货币无汇率」
    Missing,
}

#[derive(Debug, Clone, Default)]
pub struct Rates {
    map: HashMap<String, Entry>,
    /// 最近一次成功拉取的时刻
    pub fetched_at: i64,
    /// 源返回的日期，如 "2026-09-02"
    pub as_of: String,
}

impl Rates {
    pub fn new(fetched_at: i64, as_of: String) -> Self {
        Self {
            map: HashMap::new(),
            fetched_at,
            as_of,
        }
    }

    /// 插入一条汇率。**人工值不会被自动拉取的值覆盖。**
    pub fn insert(&mut self, quote: &str, rate: f64, source: Source) {
        let quote = quote.trim().to_ascii_uppercase();
        // 非法汇率一律拒绝：源异常时宁可保留旧值，也不要写进一个会让
        // 所有金额变成 0 或 inf 的数
        if !rate.is_finite() || rate <= 0.0 {
            return;
        }
        if source == Source::Frankfurter {
            if let Some(existing) = self.map.get(&quote) {
                if existing.source == Source::Manual {
                    return; // 人工值优先
                }
            }
        }
        self.map.insert(quote, Entry { rate, source });
    }

    pub fn get(&self, quote: &str) -> Option<Entry> {
        let q = quote.trim().to_ascii_uppercase();
        // USD 是基准，恒为 1
        if q == "USD" {
            return Some(Entry {
                rate: 1.0,
                source: Source::Frankfurter,
            });
        }
        self.map.get(&q).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn currencies(&self) -> Vec<String> {
        let mut v: Vec<String> = self.map.keys().cloned().collect();
        v.push("USD".into());
        v.sort();
        v.dedup();
        v
    }

    /// 把 `amount` 从 `from` 币种换算到 `to` 币种。
    ///
    /// 两边都以 USD 为基准：`amount / rate[from] * rate[to]`。
    /// 返回值同时带上降级层级，调用方据此决定 UI 怎么标注。
    pub fn convert(&self, amount: f64, from: &str, to: &str, now: i64) -> (Option<f64>, Level) {
        let (Some(f), Some(t)) = (self.get(from), self.get(to)) else {
            return (None, Level::Missing);
        };
        if f.rate <= 0.0 {
            return (None, Level::Missing);
        }
        let v = amount / f.rate * t.rate;
        if !v.is_finite() {
            return (None, Level::Missing);
        }

        // 层级取两边中更差的那个
        let level = if f.source == Source::Manual || t.source == Source::Manual {
            Level::Manual
        } else if now - self.fetched_at > STALE_AFTER_S {
            Level::Stale
        } else {
            Level::Fresh
        };
        (Some(v), level)
    }
}

/// Frankfurter 的 `/v1/latest` 响应。
///
/// 实测形状：`{"amount":1.0,"base":"USD","date":"2026-09-02","rates":{"CNY":6.7215,…}}`
#[derive(Debug, Deserialize)]
pub struct FrankfurterLatest {
    pub base: String,
    pub date: String,
    pub rates: HashMap<String, f64>,
}

impl FrankfurterLatest {
    /// 转成 [`Rates`]。**base 必须是 USD** —— 不是的话整张表的语义就变了，
    /// 与其算错不如拒绝。
    pub fn into_rates(self, fetched_at: i64) -> Option<Rates> {
        if !self.base.eq_ignore_ascii_case("USD") {
            return None;
        }
        let mut r = Rates::new(fetched_at, self.date);
        for (k, v) in self.rates {
            r.insert(&k, v, Source::Frankfurter);
        }
        Some(r)
    }
}

/// 金额按目标货币的最小单位四舍五入，用于展示。
///
/// 这不是记账系统而是估值展示：200 个 f64 求和的误差量级是 1e-13，
/// 对「总价值 ¥8,432.15」这样的展示完全无影响。
pub fn round_for_display(amount: f64, currency: &str) -> f64 {
    // 零小数位的货币
    let digits = match currency.trim().to_ascii_uppercase().as_str() {
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "HUF" => 0,
        _ => 2,
    };
    let f = 10f64.powi(digits);
    (amount * f).round() / f
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn rates() -> Rates {
        let mut r = Rates::new(NOW, "2026-09-02".into());
        // 实测值
        r.insert("CNY", 6.7215, Source::Frankfurter);
        r.insert("EUR", 0.86371, Source::Frankfurter);
        r.insert("JPY", 159.6, Source::Frankfurter);
        r
    }

    #[test]
    fn usd_is_the_base_and_always_available() {
        let r = Rates::default();
        assert_eq!(r.get("USD").unwrap().rate, 1.0);
        assert_eq!(r.convert(10.0, "USD", "USD", NOW).0, Some(10.0));
    }

    #[test]
    fn conversion_goes_through_usd() {
        let r = rates();
        // 1 USD = 6.7215 CNY
        let (v, l) = r.convert(1.0, "USD", "CNY", NOW);
        assert!((v.unwrap() - 6.7215).abs() < 1e-9);
        assert_eq!(l, Level::Fresh);

        // 反向
        let (v, _) = r.convert(6.7215, "CNY", "USD", NOW);
        assert!((v.unwrap() - 1.0).abs() < 1e-9);

        // 交叉：CNY → JPY 经由 USD
        let (v, _) = r.convert(6.7215, "CNY", "JPY", NOW);
        assert!((v.unwrap() - 159.6).abs() < 1e-6, "实际 {v:?}");
    }

    #[test]
    fn currency_code_is_case_insensitive_and_trimmed() {
        let r = rates();
        assert!(r.convert(1.0, " usd ", "cny", NOW).0.is_some());
        assert!(r.get("Cny").is_some());
    }

    // ── 四级降级 ──

    #[test]
    fn level_fresh_when_recently_fetched() {
        assert_eq!(rates().convert(1.0, "USD", "CNY", NOW).1, Level::Fresh);
    }

    #[test]
    fn level_stale_when_fetch_is_old() {
        let r = rates();
        // 源每工作日更新一次、周末不变，所以 48 小时才算陈旧
        assert_eq!(
            r.convert(1.0, "USD", "CNY", NOW + STALE_AFTER_S - 1).1,
            Level::Fresh
        );
        assert_eq!(
            r.convert(1.0, "USD", "CNY", NOW + STALE_AFTER_S + 1).1,
            Level::Stale
        );
    }

    #[test]
    fn level_manual_for_currencies_the_source_does_not_cover() {
        // 实测确认：RUB 不在 Frankfurter 的 30 种货币里
        let mut r = rates();
        assert_eq!(r.convert(1.0, "USD", "RUB", NOW).1, Level::Missing);

        r.insert("RUB", 95.0, Source::Manual);
        let (v, l) = r.convert(1.0, "USD", "RUB", NOW);
        assert_eq!(v, Some(95.0));
        assert_eq!(l, Level::Manual, "人工汇率要在 UI 上标出来");
    }

    #[test]
    fn level_missing_means_the_server_is_excluded_from_totals() {
        // 既无缓存也无人工值：不能默默当成 0，调用方会把它计入 no_rate_servers
        let (v, l) = rates().convert(1.0, "USD", "VND", NOW);
        assert_eq!(v, None);
        assert_eq!(l, Level::Missing);
    }

    #[test]
    fn manual_rate_is_not_overwritten_by_auto_fetch() {
        // 这是人工汇率存在的意义：用户填了就该一直有效，
        // 不能被下一次自动拉取悄悄冲掉
        let mut r = rates();
        r.insert("CNY", 7.5, Source::Manual);
        r.insert("CNY", 6.7215, Source::Frankfurter); // 自动拉取
        assert_eq!(r.get("CNY").unwrap().rate, 7.5, "人工值必须优先");
        assert_eq!(r.get("CNY").unwrap().source, Source::Manual);

        // 但人工值可以被新的人工值覆盖
        r.insert("CNY", 7.2, Source::Manual);
        assert_eq!(r.get("CNY").unwrap().rate, 7.2);
    }

    // ── 异常输入 ──

    #[test]
    fn invalid_rates_are_rejected_not_stored() {
        // 源异常时宁可保留旧值，也不要写进一个会让所有金额变成 0/inf 的数
        let mut r = rates();
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            r.insert("GBP", bad, Source::Frankfurter);
        }
        assert!(r.get("GBP").is_none(), "非法汇率不该被存进去");

        // 已有的合法值不受影响
        assert!((r.get("CNY").unwrap().rate - 6.7215).abs() < 1e-9);
    }

    #[test]
    fn conversion_never_returns_nan_or_infinity() {
        let mut r = Rates::new(NOW, "x".into());
        // 直接塞一个病态值绕过 insert 的校验，模拟数据库被改坏
        r.map.insert(
            "BAD".into(),
            Entry {
                rate: f64::MIN_POSITIVE,
                source: Source::Frankfurter,
            },
        );
        let (v, l) = r.convert(f64::MAX, "BAD", "USD", NOW);
        assert_eq!(v, None, "溢出成 inf 时必须返回 None 而不是一个天文数字");
        assert_eq!(l, Level::Missing);
    }

    // ── Frankfurter 解析 ──

    #[test]
    fn parses_the_real_response_shape() {
        // 实测响应（2026-09-02）
        let json = r#"{"amount":1.0,"base":"USD","date":"2026-09-02",
                       "rates":{"CNY":6.7215,"EUR":0.86371,"JPY":159.6}}"#;
        let p: FrankfurterLatest = serde_json::from_str(json).unwrap();
        let r = p.into_rates(NOW).unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r.as_of, "2026-09-02");
        assert!((r.get("CNY").unwrap().rate - 6.7215).abs() < 1e-9);
    }

    #[test]
    fn rejects_a_response_with_a_different_base() {
        // base 不是 USD 时整张表的语义就变了 —— 与其算错不如拒绝
        let json = r#"{"amount":1.0,"base":"EUR","date":"2026-09-02","rates":{"USD":1.15}}"#;
        let p: FrankfurterLatest = serde_json::from_str(json).unwrap();
        assert!(p.into_rates(NOW).is_none());
    }

    // ── 展示取整 ──

    #[test]
    fn display_rounding_respects_currency_minor_units() {
        assert_eq!(round_for_display(8432.1549, "CNY"), 8432.15);
        assert_eq!(round_for_display(8432.1549, "USD"), 8432.15);
        // 日元与韩元没有小数位
        assert_eq!(round_for_display(159.6, "JPY"), 160.0);
        assert_eq!(round_for_display(1234.7, "KRW"), 1235.0);
        assert_eq!(round_for_display(1234.7, "jpy"), 1235.0, "大小写不敏感");
    }
}
