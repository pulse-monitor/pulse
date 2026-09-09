//! 账单 / 流量 / 套餐 / 分组 / 汇率的管理接口（M5）。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use tracing::{info, warn};

use super::{Admin, ApiError, ApiResult, Ctx};
use crate::domain::billing::Cycle;
use crate::domain::rate::Source;
use crate::domain::traffic::CalcMode;
use crate::state::now_unix;
use crate::store::{BillingInput, GroupRow, PlanRow, ServerId, TrafficConfig};

pub fn routes() -> Router<Ctx> {
    Router::new()
        .route(
            "/api/v1/admin/servers/{id}/billing",
            get(get_billing).put(set_billing).delete(delete_billing),
        )
        .route(
            "/api/v1/admin/servers/{id}/traffic-config",
            get(get_traffic_config).put(set_traffic_config),
        )
        .route(
            "/api/v1/admin/servers/{id}/reset-traffic",
            post(reset_traffic),
        )
        .route("/api/v1/admin/plans", get(list_plans).post(create_plan))
        .route(
            "/api/v1/admin/plans/{id}",
            put(update_plan).delete(delete_plan),
        )
        .route("/api/v1/admin/groups", get(list_groups).post(create_group))
        .route(
            "/api/v1/admin/groups/{id}",
            put(update_group).delete(delete_group),
        )
        .route(
            "/api/v1/admin/exchange-rates",
            get(list_rates).put(set_manual_rate),
        )
        .route(
            "/api/v1/admin/exchange-rates/{quote}",
            axum::routing::delete(del_manual_rate),
        )
        .route("/api/v1/admin/exchange-rates/refresh", post(refresh_rates))
}

// ---------------------------------------------------------------------------
// 账单
// ---------------------------------------------------------------------------

fn validate_billing(b: &BillingInput) -> ApiResult<()> {
    // 价格的三种语义（见 domain::billing::is_free / is_unpriced）：
    //   -1 = 免费（白嫖来的机器，前端标「免费」徽章）
    //    0 = 未填 / 不公开价格，前端什么都不显示
    //   >0 = 正常金额
    // 除 -1 外不接受其它负数：那多半是填错了，静默收下会算出负的总价值。
    if !b.price.is_finite() || (b.price < 0.0 && b.price != -1.0) {
        return Err(ApiError::bad_request(
            "价格必须 ≥ 0；-1 表示免费，0 表示不显示价格",
        ));
    }
    if b.currency.len() != 3 || !b.currency.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(ApiError::bad_request(
            "货币必须是 3 位 ISO 4217 代码，如 USD / CNY",
        ));
    }
    let Some(cycle) = Cycle::parse(&b.cycle) else {
        return Err(ApiError::bad_request("非法的计费周期"));
    };
    if cycle == Cycle::Custom && b.custom_cycle_days.is_none_or(|d| d < 1) {
        return Err(ApiError::bad_request("自定义周期必须指定 ≥1 的天数"));
    }
    // 买断没有到期概念；其余周期没有到期时间会让剩余价值无法计算 ——
    // 允许留空，但汇总里会如实计入「N 台未设到期时间」
    if cycle != Cycle::Onetime {
        if let (Some(s), Some(e)) = (b.cycle_start_at, b.expire_at) {
            if s >= e {
                return Err(ApiError::bad_request("周期起点必须早于到期时间"));
            }
        }
    }
    Ok(())
}

async fn get_billing(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<ServerId>,
) -> ApiResult<impl IntoResponse> {
    let b = ctx
        .store
        .get_billing(id)
        .await
        .map_err(|e| ApiError::internal("查询账单", e))?;
    let now = now_unix();
    Ok(Json(serde_json::json!({
        "billing": b.as_ref().map(|b| serde_json::json!({
            "price": b.price, "currency": b.currency, "cycle": b.cycle.as_str(),
            "custom_cycle_days": b.custom_cycle_days,
            "cycle_start_at": b.cycle_start_at, "expire_at": b.expire_at,
            "auto_renew": b.auto_renew,
            // 必须回显：后台加载后原样提交，漏一个字段就等于把它清空
            "purchased_at": b.purchased_at, "remark": b.remark,
        })),
        "remaining": b.as_ref().map(|b| crate::domain::billing::remaining(b, now)),
        "renew_state": b.as_ref().map(|b| crate::domain::billing::renew_state(b, now)),
    })))
}

async fn set_billing(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<ServerId>,
    Json(b): Json<BillingInput>,
) -> ApiResult<StatusCode> {
    validate_billing(&b)?;
    if ctx.store.get_server(id).await.ok().flatten().is_none() {
        return Err(ApiError::not_found("服务器不存在"));
    }
    ctx.store
        .set_billing(id, &b, now_unix())
        .await
        .map_err(|e| ApiError::internal("保存账单", e))?;
    info!(id, price = b.price, currency = %b.currency, cycle = %b.cycle, "更新账单");
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_billing(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<ServerId>,
) -> ApiResult<StatusCode> {
    ctx.store
        .delete_billing(id)
        .await
        .map_err(|e| ApiError::internal("删除账单", e))?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// 流量
// ---------------------------------------------------------------------------

async fn get_traffic_config(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<ServerId>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        ctx.store
            .get_traffic_config(id)
            .await
            .map_err(|e| ApiError::internal("查询流量配置", e))?,
    ))
}

async fn set_traffic_config(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<ServerId>,
    Json(c): Json<TrafficConfig>,
) -> ApiResult<StatusCode> {
    if !(1..=31).contains(&c.reset_day) {
        return Err(ApiError::bad_request(
            "重置日需在 1..=31 之间（31 表示当月最后一天）",
        ));
    }
    if c.limit_bytes.is_some_and(|v| v < 0) {
        return Err(ApiError::bad_request("流量上限不能为负数（留空表示无限）"));
    }
    if let Some(tz) = &c.timezone {
        if tz.parse::<chrono_tz::Tz>().is_err() {
            return Err(ApiError::bad_request("非法的时区名，如 Asia/Shanghai"));
        }
    }
    if c.alert_pct.iter().any(|p| *p == 0 || *p > 200) {
        return Err(ApiError::bad_request("告警档位需在 1..=200 之间"));
    }
    // calc_mode 非法值在读取时会回落 sum，但这里直接拒绝，
    // 免得用户以为自己设成功了
    if CalcMode::parse(&c.calc_mode).as_str() != c.calc_mode {
        return Err(ApiError::bad_request(
            "统计口径只能是 sum / max / min / upload / download",
        ));
    }
    ctx.store
        .set_traffic_config(id, &c)
        .await
        .map_err(|e| ApiError::internal("保存流量配置", e))?;
    info!(id, mode = %c.calc_mode, reset_day = c.reset_day, "更新流量配置");
    Ok(StatusCode::NO_CONTENT)
}

async fn reset_traffic(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<ServerId>,
) -> ApiResult<StatusCode> {
    let mut row = ctx
        .store
        .get_traffic(id)
        .await
        .map_err(|e| ApiError::internal("查询流量", e))?
        .unwrap_or_default();
    row.server_id = id;
    row.in_bytes = 0;
    row.out_bytes = 0;
    // 基线也要清：否则下一轮会把「重置前累积的那一段」重新算进来
    row.last_raw_in = None;
    row.last_raw_out = None;
    row.alerted_pct.clear();
    ctx.store
        .upsert_traffic(&row, now_unix())
        .await
        .map_err(|e| ApiError::internal("重置流量", e))?;
    warn!(id, "已手动重置流量计数");
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// 套餐 / 分组
// ---------------------------------------------------------------------------

async fn list_plans(_: Admin, State(ctx): State<Ctx>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        ctx.store
            .list_plans()
            .await
            .map_err(|e| ApiError::internal("列出套餐", e))?,
    ))
}

async fn create_plan(
    _: Admin,
    State(ctx): State<Ctx>,
    Json(p): Json<PlanRow>,
) -> ApiResult<impl IntoResponse> {
    if p.name.trim().is_empty() || p.name.len() > 128 {
        return Err(ApiError::bad_request("名称需为 1..=128 个字符"));
    }
    let row = ctx
        .store
        .create_plan(&p, now_unix())
        .await
        .map_err(|e| ApiError::db("创建套餐", "已有同名套餐", e))?;
    Ok((StatusCode::CREATED, Json(row)))
}

async fn update_plan(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<i64>,
    Json(p): Json<PlanRow>,
) -> ApiResult<StatusCode> {
    if ctx
        .store
        .update_plan(id, &p)
        .await
        .map_err(|e| ApiError::db("更新套餐", "已有同名套餐", e))?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("套餐不存在"))
    }
}

async fn delete_plan(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    // 外键 ON DELETE SET NULL：删套餐不会删机器
    if ctx
        .store
        .delete_plan(id)
        .await
        .map_err(|e| ApiError::internal("删除套餐", e))?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("套餐不存在"))
    }
}

async fn list_groups(_: Admin, State(ctx): State<Ctx>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        ctx.store
            .list_groups()
            .await
            .map_err(|e| ApiError::internal("列出分组", e))?,
    ))
}

async fn create_group(
    _: Admin,
    State(ctx): State<Ctx>,
    Json(g): Json<GroupRow>,
) -> ApiResult<impl IntoResponse> {
    if g.name.trim().is_empty() || g.name.len() > 64 {
        return Err(ApiError::bad_request("名称需为 1..=64 个字符"));
    }
    let row = ctx
        .store
        .create_group(&g, now_unix())
        .await
        .map_err(|e| ApiError::db("创建分组", "已有同名分组", e))?;
    Ok((StatusCode::CREATED, Json(row)))
}

async fn update_group(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<i64>,
    Json(g): Json<GroupRow>,
) -> ApiResult<StatusCode> {
    if ctx
        .store
        .update_group(id, &g)
        .await
        .map_err(|e| ApiError::db("更新分组", "已有同名分组", e))?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("分组不存在"))
    }
}

async fn delete_group(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    // 外键 ON DELETE SET NULL：机器变成「未分组」而不是被删掉
    if ctx
        .store
        .delete_group(id)
        .await
        .map_err(|e| ApiError::internal("删除分组", e))?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("分组不存在"))
    }
}

// ---------------------------------------------------------------------------
// 汇率
// ---------------------------------------------------------------------------

async fn list_rates(_: Admin, State(ctx): State<Ctx>) -> ApiResult<impl IntoResponse> {
    let r = ctx
        .store
        .load_rates()
        .await
        .map_err(|e| ApiError::internal("读取汇率", e))?;
    let now = now_unix();
    let items: Vec<_> = r
        .currencies()
        .into_iter()
        .filter_map(|q| {
            let e = r.get(&q)?;
            Some(serde_json::json!({
                "quote": q, "rate": e.rate,
                "source": if e.source == Source::Manual { "manual" } else { "frankfurter" },
            }))
        })
        .collect();
    Ok(Json(serde_json::json!({
        "as_of": r.as_of,
        "fetched_at": r.fetched_at,
        "stale": now - r.fetched_at > crate::domain::rate::STALE_AFTER_S,
        "rates": items,
        // 实测：源只覆盖 30 种货币，这几个 VPS 场景常见的都要人工填
        "known_unsupported": ["RUB", "TWD", "VND", "UAH", "ARS", "AED"],
    })))
}

#[derive(Deserialize)]
pub struct ManualRate {
    quote: String,
    rate: f64,
}

async fn set_manual_rate(
    _: Admin,
    State(ctx): State<Ctx>,
    Json(m): Json<ManualRate>,
) -> ApiResult<StatusCode> {
    if m.quote.len() != 3 || !m.quote.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(ApiError::bad_request("货币必须是 3 位 ISO 4217 代码"));
    }
    if !m.rate.is_finite() || m.rate <= 0.0 {
        return Err(ApiError::bad_request("汇率必须是正数（1 USD = ? 该货币）"));
    }
    ctx.store
        .set_manual_rate(&m.quote, m.rate, now_unix())
        .await
        .map_err(|e| ApiError::internal("保存人工汇率", e))?;
    info!(quote = %m.quote, rate = m.rate, "设置人工汇率");
    Ok(StatusCode::NO_CONTENT)
}

async fn del_manual_rate(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(quote): Path<String>,
) -> ApiResult<StatusCode> {
    if ctx
        .store
        .delete_manual_rate(&quote)
        .await
        .map_err(|e| ApiError::internal("删除人工汇率", e))?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("该货币没有人工汇率"))
    }
}

async fn refresh_rates(_: Admin, State(ctx): State<Ctx>) -> ApiResult<impl IntoResponse> {
    crate::tasks::rate::fetch_once(&ctx.store, &ctx.config.rate_base_url, now_unix())
        .await
        .map_err(|e| ApiError::internal("拉取汇率", e))?;
    let r = ctx
        .store
        .load_rates()
        .await
        .map_err(|e| ApiError::internal("读取汇率", e))?;
    Ok(Json(
        serde_json::json!({ "currencies": r.len(), "as_of": r.as_of }),
    ))
}
