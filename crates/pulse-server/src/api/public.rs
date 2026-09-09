//! 公开只读接口。受站点 `public_mode` 控制。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use super::{ApiError, ApiResult, Ctx, PublicMode};
use crate::state::now_unix;
use crate::store::ServerId;

/// 安装脚本内嵌进二进制，这样 `curl https://panel/install.sh | sudo bash` 就能用，
/// 不需要额外的静态文件服务。脚本在仓库里可审计，也鼓励用户先下载看一眼再执行。
const INSTALL_SH: &str = include_str!("../../../../deploy/scripts/install.sh");
const INSTALL_PS1: &str = include_str!("../../../../deploy/windows/install-service.ps1");

pub fn routes() -> Router<Ctx> {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/install.sh", get(install_sh))
        .route("/install.ps1", get(install_ps1))
        .route("/api/v1/public/servers", get(list))
        .route("/api/v1/public/summary", get(summary))
        .route("/api/v1/public/expiring", get(expiring))
        .route("/api/v1/public/groups", get(groups))
        .route("/api/v1/public/servers/{uuid}/metrics", get(metrics))
        .route("/api/v1/public/servers/{uuid}/ping", get(ping))
        .route("/api/v1/public/servers/{uuid}/ping-tasks", get(ping_tasks))
}

/// 私有模式下未登录直接 401。
fn gate(ctx: &Ctx) -> ApiResult<()> {
    match ctx.config.public_mode {
        PublicMode::Public => Ok(()),
        PublicMode::Private => Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "PRIVATE",
            "本站为私有模式，请先登录",
        )),
    }
}

/// 安装脚本。**不需要认证** —— 脚本本身不含任何秘密，
/// token 是用户从安装命令里带进来的参数。
async fn install_sh() -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/x-shellscript; charset=utf-8",
        )],
        INSTALL_SH,
    )
}

async fn install_ps1() -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        INSTALL_PS1,
    )
}

/// 不需要认证：负载均衡与容器健康检查要用。
/// **不返回任何内部细节** —— 详细状态在 /api/v1/admin/health，那个要 JWT。
async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

async fn list(State(ctx): State<Ctx>) -> ApiResult<impl IntoResponse> {
    gate(&ctx)?;
    // 首页快照全部来自内存，**0 次数据库查询** —— 这是设计约束，不是优化。
    //
    // **以数据库的机器列表为准**（价值缓存），再合并实时层。
    // 只列内存里的话，一台刚添加、还没装探针的机器在面板上完全看不见 ——
    // 而那正是你最想确认它状态的时候。
    let values = ctx.state.values();
    let live: std::collections::HashMap<_, _> = ctx
        .state
        .list()
        .into_iter()
        .map(|s| (s.id.clone(), s))
        .collect();

    let out: Vec<_> = values
        .servers
        .iter()
        .filter(|v| !v.hidden)
        .map(|v| {
            let s = live
                .get(&v.uuid)
                .cloned()
                .unwrap_or_else(|| v.offline_view());
            serde_json::json!({
                "server": s,
                "billing": v.billing.as_ref(),
                "traffic": &v.traffic,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({
        "servers": out,
        "display_currency": values.display_currency,
    })))
}

async fn summary(State(ctx): State<Ctx>) -> ApiResult<impl IntoResponse> {
    gate(&ctx)?;
    let s = ctx.state.summary();
    let v = ctx.state.values();
    let net = ctx.state.total_speed();

    Ok(Json(serde_json::json!({
        "servers": { "total": s.total, "online": s.online, "offline": s.offline },
        "network": { "in_speed": net.0, "out_speed": net.1 },
        "value": {
            "display_currency": v.display_currency,
            // 「全部续一次费要花多少」—— 用户要的「总价值」
            "total_value": v.totals.total_value,
            // 唯一可横向比较的口径，UI 放 tooltip
            "annual_cost": v.totals.annual_cost,
            "total_remaining": v.totals.total_remaining,
            // 三个**诚实性字段**：汇总不完整时必须说清少算了几台，
            // 而不是默默当成 0
            "unpriced_servers": v.totals.unpriced_servers,
            "no_expire_servers": v.totals.no_expire_servers,
            "no_rate_servers": v.totals.no_rate_servers,
        },
        "rate": {
            "as_of": v.rate_as_of,
            "fetched_at": v.rate_fetched_at,
            "stale": v.rate_stale,
        },
        "updated_at": v.updated_at,
    })))
}

#[derive(Debug, Deserialize)]
pub struct ExpiringQuery {
    #[serde(default = "default_days")]
    days: i64,
}

fn default_days() -> i64 {
    7
}

/// 即将到期的机器（R13）。按剩余天数升序。
///
/// **已过期的也要列出来**（剩余天数为负），而不是让它们消失 ——
/// 一台悄悄过期的机器正是最需要被看见的。
async fn expiring(
    State(ctx): State<Ctx>,
    Query(q): Query<ExpiringQuery>,
) -> ApiResult<impl IntoResponse> {
    gate(&ctx)?;
    let days = q.days.clamp(1, 365);
    let values = ctx.state.values();

    let mut items: Vec<_> = values
        .servers
        .iter()
        .filter_map(|v| {
            let b = v.billing.as_ref()?;
            let d = b.remain_days?;
            (d <= days).then(|| {
                serde_json::json!({
                    "id": v.uuid,
                    "name": v.name,
                    "country_code": v.country_code,
                    "remain_days": d,
                    "expire_at": b.expire_at,
                    "auto_renew": b.auto_renew,
                    // 开了自动续费且在宽限期内 → 前端显示「续费中」而不是告警
                    "renew_state": b.renew_state,
                    "remaining_display": b.remaining_display,
                })
            })
        })
        .collect();
    items.sort_by_key(|v| v["remain_days"].as_i64().unwrap_or(i64::MAX));

    Ok(Json(serde_json::json!({
        "days": days,
        "display_currency": values.display_currency,
        "items": items,
    })))
}

/// 分组列表（R11）。含每组的机器数与在线数。
async fn groups(State(ctx): State<Ctx>) -> ApiResult<impl IntoResponse> {
    gate(&ctx)?;
    // 分组是低频数据，这里查一次库是可以接受的 —— 它不在首页的热路径上
    let gs = ctx
        .store
        .list_groups()
        .await
        .map_err(|e| ApiError::internal("列出分组", e))?;
    Ok(Json(gs))
}

#[derive(Debug, Deserialize)]
pub struct RangeQuery {
    /// `1h` / `6h` / `24h` / `7d` / `30d` / `90d` / `1y`
    range: Option<String>,
    from: Option<i64>,
    to: Option<i64>,
    task_id: Option<i64>,
}

impl RangeQuery {
    /// 解析成 `(from, to)`。非法输入回落到默认 6 小时而不是报错 ——
    /// 图表接口对坏参数应当降级，不该给用户一个红色错误页。
    fn resolve(&self) -> (i64, i64) {
        let now = now_unix();
        if let (Some(f), Some(t)) = (self.from, self.to) {
            if t > f {
                return (f, t);
            }
        }
        let secs = match self.range.as_deref() {
            Some("1h") => 3_600,
            Some("6h") => 21_600,
            Some("24h") => 86_400,
            Some("7d") => 604_800,
            Some("30d") => 2_592_000,
            Some("90d") => 7_776_000,
            Some("1y") => 31_536_000,
            _ => 21_600,
        };
        (now - secs, now)
    }
}

/// uuid → 数据库主键。**先查内存，查不到再查库**。
///
/// 内存里的 `AppState` 只装本进程见过的会话，拿它当唯一来源会有两个后果：
/// - 面板刚重启完，所有机器的详情页都 404，直到各自的探针重连（退避最长 5 分钟）
/// - 从没装过探针的机器**永远**打不开 —— 哪怕账单、备注、历史指标都在库里
///
/// 真机上就是这么发现的：点开卡片，metrics 和 ping 全是 404。
///
/// 保留内存这一层是因为它命中率极高（在线机器都在里面），
/// 省掉每次看图都要走一次数据库。
async fn resolve_id(ctx: &Ctx, uuid: &str) -> Result<ServerId, ApiError> {
    if let Some(id) = ctx.state.db_id(uuid) {
        return Ok(id);
    }
    ctx.store
        .get_server_by_uuid(uuid)
        .await
        .map_err(|e| ApiError::internal("查询服务器", e))?
        .map(|s| s.id)
        .ok_or_else(|| ApiError::not_found("服务器不存在"))
}

async fn metrics(
    State(ctx): State<Ctx>,
    Path(uuid): Path<String>,
    Query(q): Query<RangeQuery>,
) -> ApiResult<impl IntoResponse> {
    gate(&ctx)?;
    let id = resolve_id(&ctx, &uuid).await?;
    let (from, to) = q.resolve();
    ctx.store
        .query_metrics(id, from, to)
        .await
        .map(Json)
        .map_err(|e| ApiError::internal("查询指标", e))
}

/// 这台机器**实际生效**的延迟任务（id + 名称）。
///
/// 前端要把多个探测点画到同一张图上，得先知道有哪几个、各叫什么。
/// 之前是盲猜 task_id 1..5 各请求一次 —— 猜不到名字（只能显示「延迟监测 #1」），
/// 任务超过 5 个还会漏。
///
/// 只暴露 id 和名称：host、端口、间隔这些是运维配置，公开页面没有理由知道。
async fn ping_tasks(
    State(ctx): State<Ctx>,
    Path(uuid): Path<String>,
) -> ApiResult<impl IntoResponse> {
    gate(&ctx)?;
    let id = resolve_id(&ctx, &uuid).await?;
    let tasks = ctx
        .store
        .ping_tasks_for_server(id)
        .await
        .map_err(|e| ApiError::internal("查询延迟任务", e))?;
    let out: Vec<_> = tasks
        .iter()
        .map(|t| serde_json::json!({ "id": t.id, "name": t.name }))
        .collect();
    Ok(Json(out))
}

async fn ping(
    State(ctx): State<Ctx>,
    Path(uuid): Path<String>,
    Query(q): Query<RangeQuery>,
) -> ApiResult<impl IntoResponse> {
    gate(&ctx)?;
    let id = resolve_id(&ctx, &uuid).await?;
    let (from, to) = q.resolve();
    ctx.store
        .query_ping(id, q.task_id.unwrap_or(1), from, to)
        .await
        .map(Json)
        .map_err(|e| ApiError::internal("查询延迟", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(range: Option<&str>) -> RangeQuery {
        RangeQuery {
            range: range.map(str::to_string),
            from: None,
            to: None,
            task_id: None,
        }
    }

    #[test]
    fn range_shorthands_resolve() {
        let (f, t) = q(Some("24h")).resolve();
        assert_eq!(t - f, 86_400);
        let (f, t) = q(Some("30d")).resolve();
        assert_eq!(t - f, 2_592_000);
    }

    #[test]
    fn unknown_or_missing_range_falls_back_to_six_hours() {
        // 图表接口对坏参数应当降级而不是报错
        for bad in [None, Some(""), Some("garbage"), Some("99y")] {
            let (f, t) = q(bad).resolve();
            assert_eq!(t - f, 21_600, "{bad:?} 应当回落到 6 小时");
        }
    }

    #[test]
    fn explicit_from_to_wins_but_must_be_ordered() {
        let ok = RangeQuery {
            range: None,
            from: Some(100),
            to: Some(200),
            task_id: None,
        };
        assert_eq!(ok.resolve(), (100, 200));

        // 起止颠倒时忽略，回落默认 —— 否则会算出负跨度
        let bad = RangeQuery {
            range: None,
            from: Some(200),
            to: Some(100),
            task_id: None,
        };
        let (f, t) = bad.resolve();
        assert_eq!(t - f, 21_600);
    }
}
