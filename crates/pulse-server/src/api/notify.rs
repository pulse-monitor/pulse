//! 通知渠道与规则的管理接口（M6）。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use tracing::{info, warn};

use super::{Admin, ApiError, ApiResult, Ctx};
use crate::domain::alert::EventKind;
use crate::notify::{self, NotifyMessage};
use crate::state::now_unix;
use crate::store::{ChannelInput, RuleRow};

pub fn routes() -> Router<Ctx> {
    Router::new()
        .route(
            "/api/v1/admin/notification-channels",
            get(list_channels).post(create_channel),
        )
        .route(
            "/api/v1/admin/notification-channels/{id}",
            put(update_channel).delete(delete_channel),
        )
        .route(
            "/api/v1/admin/notification-channels/{id}/test",
            post(test_channel),
        )
        .route(
            "/api/v1/admin/notification-rules",
            get(list_rules).post(create_rule),
        )
        .route(
            "/api/v1/admin/notification-rules/{id}",
            put(update_rule).delete(delete_rule),
        )
        .route("/api/v1/admin/events", get(list_alerts))
        .route("/api/v1/admin/event-kinds", get(list_event_kinds))
}

// ---------------------------------------------------------------------------
// 渠道
// ---------------------------------------------------------------------------

async fn list_channels(_: Admin, State(ctx): State<Ctx>) -> ApiResult<impl IntoResponse> {
    let cs = ctx
        .store
        .list_channels(&ctx.secret)
        .await
        .map_err(|e| ApiError::internal("列出渠道", e))?;
    // **凭据一律掩码后再回显** —— 用户只需要确认「填过了」
    let out: Vec<_> = cs
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id, "name": c.name, "kind": c.kind,
                "enabled": c.enabled, "config": c.config.masked(),
            })
        })
        .collect();
    Ok(Json(out))
}

fn validate_channel(c: &ChannelInput) -> ApiResult<()> {
    if c.name.trim().is_empty() || c.name.len() > 64 {
        return Err(ApiError::bad_request("名称需为 1..=64 个字符"));
    }
    match &c.config {
        notify::ChannelConfig::Email(e) => {
            if e.host.trim().is_empty() || e.from.trim().is_empty() {
                return Err(ApiError::bad_request("SMTP 主机与发件人不能为空"));
            }
            if e.to.is_empty() {
                return Err(ApiError::bad_request("至少需要一个收件人"));
            }
        }
        notify::ChannelConfig::Telegram(t) => {
            if !t.token.contains(':') {
                return Err(ApiError::bad_request("Telegram token 形如 123456:AA..."));
            }
            if t.chat_id.trim().is_empty() {
                return Err(ApiError::bad_request("chat_id 不能为空"));
            }
        }
        notify::ChannelConfig::Wecom(w) => {
            if w.key.trim().is_empty() {
                return Err(ApiError::bad_request("企业微信 webhook key 不能为空"));
            }
        }
        notify::ChannelConfig::Lark(l) => {
            if !notify::is_acceptable_webhook(&l.webhook) {
                return Err(ApiError::bad_request(
                    "飞书 webhook 必须是 https:// 完整地址（内网/回环地址可以用 http://）",
                ));
            }
        }
    }
    Ok(())
}

async fn create_channel(
    _: Admin,
    State(ctx): State<Ctx>,
    Json(c): Json<ChannelInput>,
) -> ApiResult<impl IntoResponse> {
    validate_channel(&c)?;
    let id = ctx
        .store
        .create_channel(&c, &ctx.secret, now_unix())
        .await
        .map_err(|e| ApiError::db("创建渠道", "已有同名通知渠道", e))?;
    info!(id, kind = c.config.kind().as_str(), "创建通知渠道");
    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": id }))))
}

async fn update_channel(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<i64>,
    Json(c): Json<ChannelInput>,
) -> ApiResult<StatusCode> {
    validate_channel(&c)?;
    if ctx
        .store
        .update_channel(id, &c, &ctx.secret)
        .await
        .map_err(|e| ApiError::db("更新渠道", "已有同名通知渠道", e))?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("渠道不存在"))
    }
}

async fn delete_channel(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    if ctx
        .store
        .delete_channel(id)
        .await
        .map_err(|e| ApiError::internal("删除渠道", e))?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("渠道不存在"))
    }
}

/// 「发送测试」按钮。
///
/// 这是渠道配置**唯一能自证的方式** —— 配错了的表现是「以后出事收不到通知」，
/// 那时候才发现就太晚了。
async fn test_channel(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<i64>,
) -> ApiResult<impl IntoResponse> {
    let cs = ctx
        .store
        .list_channels(&ctx.secret)
        .await
        .map_err(|e| ApiError::internal("读取渠道", e))?;
    let ch = cs
        .iter()
        .find(|c| c.id == id)
        .ok_or_else(|| ApiError::not_found("渠道不存在，或它的凭据解密失败"))?;

    let msg = NotifyMessage {
        title: "Pulse 测试通知".into(),
        body: format!(
            "这是一条测试消息，说明渠道「{}」配置正确。\n发送时间：{}",
            ch.name,
            chrono::DateTime::from_timestamp(now_unix(), 0)
                .map(|t| t
                    .with_timezone(&ctx.config.timezone)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string())
                .unwrap_or_default()
        ),
        resolved: false,
    };

    match notify::send(&ch.config, &msg).await {
        Ok(()) => {
            info!(id, "测试通知已发送");
            Ok(Json(serde_json::json!({ "ok": true })))
        }
        Err(e) => {
            warn!(id, "测试通知发送失败: {e}");
            // 把渠道返回的原始错误带给用户 —— Telegram 的 "chat not found"、
            // SMTP 的认证失败都很具体，吞掉它用户就不知道该改什么
            Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "SEND_FAILED",
                format!("发送失败：{e}"),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// 规则
// ---------------------------------------------------------------------------

async fn list_rules(_: Admin, State(ctx): State<Ctx>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        ctx.store
            .list_rules()
            .await
            .map_err(|e| ApiError::internal("列出规则", e))?,
    ))
}

fn validate_rule(r: &RuleRow) -> ApiResult<()> {
    if r.name.trim().is_empty() || r.name.len() > 64 {
        return Err(ApiError::bad_request("名称需为 1..=64 个字符"));
    }
    if r.event_kinds.is_empty() {
        return Err(ApiError::bad_request("至少选择一个事件类型"));
    }
    for k in &r.event_kinds {
        if EventKind::parse(k).is_none() {
            return Err(ApiError::bad_request(format!("未知的事件类型: {k}")));
        }
    }
    if r.channel_ids.is_empty() {
        return Err(ApiError::bad_request("至少绑定一个通知渠道"));
    }
    if !(0..=86_400).contains(&r.duration_s) {
        return Err(ApiError::bad_request("持续时长需在 0..=86400 秒之间"));
    }
    if !(0..=30 * 86_400).contains(&r.cooldown_s) {
        return Err(ApiError::bad_request("冷却时长需在 0..=30 天之间"));
    }
    Ok(())
}

async fn create_rule(
    _: Admin,
    State(ctx): State<Ctx>,
    Json(r): Json<RuleRow>,
) -> ApiResult<impl IntoResponse> {
    validate_rule(&r)?;
    let id = ctx
        .store
        .create_rule(&r, now_unix())
        .await
        .map_err(|e| ApiError::db("创建规则", "已有同名通知规则", e))?;
    info!(id, events = ?r.event_kinds, "创建通知规则");
    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": id }))))
}

async fn update_rule(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<i64>,
    Json(r): Json<RuleRow>,
) -> ApiResult<StatusCode> {
    validate_rule(&r)?;
    if ctx
        .store
        .update_rule(id, &r)
        .await
        .map_err(|e| ApiError::db("更新规则", "已有同名通知规则", e))?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("规则不存在"))
    }
}

async fn delete_rule(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    // 外键 CASCADE：规则的告警记录一并清理
    if ctx
        .store
        .delete_rule(id)
        .await
        .map_err(|e| ApiError::internal("删除规则", e))?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("规则不存在"))
    }
}

// ---------------------------------------------------------------------------
// 告警历史 / 元数据
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AlertQuery {
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    100
}

async fn list_alerts(
    _: Admin,
    State(ctx): State<Ctx>,
    Query(q): Query<AlertQuery>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        ctx.store
            .list_alerts(q.limit)
            .await
            .map_err(|e| ApiError::internal("列出告警历史", e))?,
    ))
}

/// 全部事件类型，供后台下拉。带默认阈值参数名，前端据此渲染表单。
async fn list_event_kinds(_: Admin) -> impl IntoResponse {
    let items: Vec<_> = EventKind::ALL
        .iter()
        .map(|k| {
            // `cmp` 是**比较方向**，必须由后端给出：
            // 「CPU 使用率过高」是 ≥ 阈值，「即将到期」是剩余天数 ≤ 阈值。
            // 之前前端把符号写死成 ≥，于是后台显示成「days ≥ 7」，意思正好反了。
            let (param, default, cmp) = match k {
                EventKind::CpuHigh => ("cpu_pct", 90.0, "gte"),
                EventKind::MemHigh => ("mem_pct", 90.0, "gte"),
                EventKind::DiskHigh => ("disk_pct", 90.0, "gte"),
                EventKind::TrafficThreshold => ("traffic_pct", 90.0, "gte"),
                EventKind::ExpireSoon => ("days", 7.0, "lte"),
                EventKind::PingLossHigh => ("loss_pct", 50.0, "gte"),
                EventKind::PingLatencyHigh => ("rtt_ms", 500.0, "gte"),
                EventKind::ServerOffline => ("", 0.0, ""),
            };
            serde_json::json!({
                "kind": k.as_str(), "label": k.label(),
                "param": (!param.is_empty()).then_some(param),
                "default": (!param.is_empty()).then_some(default),
                "cmp": (!cmp.is_empty()).then_some(cmp),
                "unit": match param {
                    "days" => "天",
                    "rtt_ms" => "ms",
                    "" => "",
                    _ => "%",
                },
            })
        })
        .collect();
    Json(items)
}
