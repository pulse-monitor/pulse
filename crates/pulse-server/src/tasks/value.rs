//! 账单/流量视图的缓存刷新。
//!
//! **为什么要缓存**：M1 定下的约束是「首页 summary 的数据库查询次数 = 0」
//! 。账单与流量是低频数据 —— 用户几个月才改一次 ——
//! 所以按固定节奏刷进内存，而不是每个请求都去查库。
//!
//! 代价是最长 30 秒的陈旧，对「剩余天数」「本月已用流量」这类量完全可接受。

use std::sync::Arc;

use tracing::debug;

use crate::domain::billing::{self, Billing};
use crate::domain::rate::{Level, Rates};
use crate::domain::traffic::{self, CalcMode};
use crate::state::{AppState, ServerValue, TrafficView, ValueSnapshot};
use crate::store::Storage;

pub async fn refresh_once(
    state: &AppState,
    store: &Arc<dyn Storage>,
    display_currency: &str,
    now: i64,
) -> anyhow::Result<()> {
    let rates = store.load_rates().await?;
    let all = store.all_billing().await?;

    let convert = |amount: f64, from: &str| -> Option<f64> {
        rates.convert(amount, from, display_currency, now).0
    };

    // 三个总价值口径
    let totals = billing::totals(all.iter().map(|e| e.billing.as_ref()), now, convert);

    let mut servers = Vec::with_capacity(all.len());
    for e in &all {
        let (id, b) = (&e.id, &e.billing);
        let cfg = store.get_traffic_config(*id).await?;
        let t = store.get_traffic(*id).await?;
        let mode = CalcMode::parse(&cfg.calc_mode);
        let (in_b, out_b) = t
            .as_ref()
            .map(|t| (t.in_bytes as u64, t.out_bytes as u64))
            .unwrap_or((0, 0));
        let used = traffic::used(in_b, out_b, mode);
        let limit = cfg.limit_bytes.map(|v| v as u64).filter(|v| *v > 0);

        servers.push(ServerValue {
            server_id: *id,
            uuid: e.uuid.clone(),
            name: e.name.clone(),
            country_code: e.country_code.clone(),
            latitude: e.latitude,
            longitude: e.longitude,
            group_id: e.group_id,
            buy_url: e.buy_url.clone(),
            review_url: e.review_url.clone(),
            hidden: e.hidden,
            os: e.os.clone(),
            arch: e.arch.clone(),
            cpu_cores: e.cpu_cores,
            mem_total: e.mem_total,
            disk_total: e.disk_total,
            agent_version: e.agent_version.clone(),
            last_seen_at: e.last_seen_at,
            billing: b
                .as_ref()
                .map(|b| billing_view(b, &rates, display_currency, now)),
            traffic: TrafficView {
                in_bytes: in_b,
                out_bytes: out_b,
                used,
                limit,
                // 无限流量时前端显示 ♾️，不是 0%
                pct: limit.map(|l| used as f64 * 100.0 / l as f64),
                calc_mode: mode.as_str(),
                period_start: t.as_ref().map(|t| t.period_start).unwrap_or(0),
                reset_day: cfg.reset_day,
            },
        });
    }

    state.set_values(ValueSnapshot {
        servers,
        totals,
        display_currency: display_currency.to_string(),
        rate_as_of: rates.as_of.clone(),
        rate_fetched_at: rates.fetched_at,
        rate_stale: now - rates.fetched_at > crate::domain::rate::STALE_AFTER_S,
        updated_at: now,
    });
    debug!(servers = all.len(), "价值缓存已刷新");
    Ok(())
}

fn billing_view(b: &Billing, rates: &Rates, display: &str, now: i64) -> crate::state::BillingView {
    let r = billing::remaining(b, now);
    let (price_display, level) = rates.convert(b.price, &b.currency, display, now);
    // 免费机器（price = -1）的剩余价值恒为 0。
    // 直接按 -1 折算会得出「剩余价值 ¥-6.71」这种明显是 bug 的数字。
    let (remaining_display, _) = if billing::is_free(b.price) {
        (Some(0.0), level)
    } else {
        rates.convert(r.value, &b.currency, display, now)
    };

    crate::state::BillingView {
        price: b.price,
        currency: b.currency.clone(),
        cycle: b.cycle.as_str(),
        expire_at: b.expire_at,
        auto_renew: b.auto_renew,
        remain_days: r.days,
        remaining_value: r.value,
        infinite: r.infinite,
        // 未设到期时间：前端要显示「—」而不是「0 天」
        unknown_expiry: r.unknown,
        price_display,
        remaining_display,
        // 汇率不可用时前端要标注，而不是显示一个凭空的数字
        rate_level: level,
        renew_state: billing::renew_state(b, now),
    }
}

/// 汇率层级是否值得在 UI 上标注。
pub fn needs_notice(l: Level) -> bool {
    !matches!(l, Level::Fresh)
}
