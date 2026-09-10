//! HTTP / WS 接口层。
//!
//! 三块互不越界：
//! - [`public`] 公开只读，受站点 `public_mode` 控制
//! - [`admin`] 管理，全部需要 JWT
//! - [`agent`] 只有一个 WS 接入点，**没有第二个** agent 能调的接口

pub mod admin;
pub mod agent;
pub mod billing;
pub mod live;
pub mod notify;
pub mod public;
pub mod visitor;

use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde::Serialize;

use crate::auth::{Jwt, LoginLimiter, TokenKind};
use crate::state::AppState;
use crate::store::Storage;
use tower_http::compression::CompressionLayer;

/// 服务端运行配置。
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// 面板对外地址，用于生成安装命令
    pub panel_url: String,
    /// **可信反向代理的层数**。
    ///
    /// 0 表示直连，此时完全忽略 `X-Forwarded-For` —— 否则任何人都能
    /// 伪造来源 IP，把登录限流和访客标签一起骗过去。
    pub trusted_proxy_hops: usize,
    pub public_mode: PublicMode,
    /// 汇率源基址
    pub rate_base_url: String,
    /// 展示币种。总价值 / 剩余价值都换算到它
    pub display_currency: String,
    /// 面板时区。账单周期用它，而不是机器本地时区
    pub timezone: chrono_tz::Tz,
    /// 访客悬浮标签（R14）。关掉时接口返回 404，前端完全不渲染
    pub visitor_badge: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicMode {
    Public,
    Private,
}

#[derive(Clone)]
pub struct Ctx {
    pub state: AppState,
    pub store: Arc<dyn Storage>,
    pub jwt: Jwt,
    pub limiter: Arc<LoginLimiter>,
    pub config: Arc<ServerConfig>,
    /// 服务端密钥。用于解密渠道凭据（与 JWT 同一把）
    pub secret: Arc<Vec<u8>>,
    /// IP → 国家。**本地查表**，IP 不出本机（见 tasks/geoip.rs）
    pub geoip: crate::tasks::geoip::SharedTable,
}

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// 统一错误体：`{"error": {"code": "...", "message": "..."}}`。
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "UNAUTHORIZED", msg)
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "NOT_FOUND", msg)
    }
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "BAD_REQUEST", msg)
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "CONFLICT", msg)
    }
    /// 内部错误：**给客户端的是一句笼统的话，细节只进日志** ——
    /// 数据库错误里常含表名、SQL 片段，不该外泄。
    pub fn internal(context: &str, e: impl std::fmt::Display) -> Self {
        tracing::error!("{context}: {e}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", "内部错误")
    }

    /// 唯一约束冲突要分出来单独报。
    ///
    /// 这类错误是**用户输入造成的**（重名），归到 500「内部错误」的话，
    /// 后台只会显示一句没头没脑的「内部错误」，用户根本不知道该改什么。
    /// `conflict_msg` 是给用户看的话，绝不能把数据库原文带出去。
    ///
    /// 泛型是因为调用点一半传 `sqlx::Error`、一半传 `anyhow::Error`。
    pub fn db<E: Into<anyhow::Error>>(context: &str, conflict_msg: &str, e: E) -> Self {
        let e: anyhow::Error = e.into();
        if is_unique_violation(&e) {
            tracing::info!("{context}: 唯一约束冲突");
            return Self::conflict(conflict_msg.to_string());
        }
        Self::internal(context, e)
    }
}

/// 这个错误是不是「唯一约束冲突」。
///
/// 只认数据库自己给的错误码，不去匹配错误信息文本 ——
/// 文本会随驱动版本和语言环境变化，匹配它迟早失灵。
fn is_unique_violation(e: &anyhow::Error) -> bool {
    for cause in e.chain() {
        if let Some(sqlx::Error::Database(db)) = cause.downcast_ref::<sqlx::Error>() {
            return db.is_unique_violation();
        }
    }
    false
}

#[derive(Serialize)]
struct ErrBody<'a> {
    error: ErrDetail<'a>,
}
#[derive(Serialize)]
struct ErrDetail<'a> {
    code: &'a str,
    message: &'a str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrBody {
                error: ErrDetail {
                    code: self.code,
                    message: &self.message,
                },
            }),
        )
            .into_response()
    }
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;

// ---------------------------------------------------------------------------
// 认证提取器
// ---------------------------------------------------------------------------

/// 出现在处理函数参数里就代表「这个接口需要管理员登录」。
///
/// 忘记加它 = 接口裸奔，所以宁可让它显眼一点。
pub struct Admin {
    #[allow(dead_code)]
    pub username: String,
}

impl FromRequestParts<Ctx> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, ctx: &Ctx) -> Result<Self, Self::Rejection> {
        let token = bearer(&parts.headers)
            .ok_or_else(|| ApiError::unauthorized("缺少 Authorization: Bearer"))?;
        let claims = ctx
            .jwt
            .verify(&token, TokenKind::Access)
            .map_err(|e| ApiError::unauthorized(format!("令牌无效: {e}")))?;
        Ok(Admin {
            username: claims.sub,
        })
    }
}

pub fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::to_owned)
        .filter(|t| !t.trim().is_empty())
}

// ---------------------------------------------------------------------------
// 客户端 IP
// ---------------------------------------------------------------------------

/// 解析真实客户端 IP。
///
/// **`hops == 0` 时完全忽略 `X-Forwarded-For`。** 这是默认值，也是唯一安全的默认：
/// 直连时任何人都能自己加一个 XFF 头，把登录限流和访客标签一起骗过去。
///
/// `hops = N` 表示前面有 N 层可信代理，取 XFF 列表**从右往左**第 N 个 ——
/// 右边的是离我们最近、最可信的那些。取最左边（很多实现的做法）是错的：
/// 最左边完全由客户端控制。
pub fn client_ip(headers: &HeaderMap, peer: IpAddr, hops: usize) -> IpAddr {
    if hops == 0 {
        return peer;
    }
    let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) else {
        return peer;
    };
    let list: Vec<&str> = xff
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    // 代理链：[client, proxy1, ..., proxyN-1]，我们要的是倒数第 hops 个
    list.len()
        .checked_sub(hops)
        .and_then(|i| list.get(i))
        .and_then(|s| s.parse().ok())
        .unwrap_or(peer)
}

// ---------------------------------------------------------------------------
// 路由
// ---------------------------------------------------------------------------

/// 静态前端。
///
/// 架构上是**前后端分离**，但让 server 顺便把
/// 构建产物也服务了，就能得到「一个二进制 + 一个 db 文件」的部署形态 ——
/// 两种模式并不冲突，用 `PULSE_WEB_DIR` 控制。
///
/// SPA 需要 fallback：客户端路由的 `/server/xxx` 在磁盘上没有对应文件，
/// 必须回到 index.html 由前端接管，否则刷新页面就是 404。
fn static_files(dir: &str) -> Router<Ctx> {
    use axum::http::{header, HeaderValue};
    use tower_http::services::{ServeDir, ServeFile};
    use tower_http::set_header::SetResponseHeaderLayer;
    let index = std::path::Path::new(dir).join("index.html");
    if !index.exists() {
        // 没构建前端时给一句人话，而不是一个空白的 404
        return Router::new().route(
            "/",
            axum::routing::get(|| async {
                axum::response::Html(
                    "<!doctype html><meta charset=utf-8><title>Pulse</title>\
                     <p style=\"font:14px system-ui;padding:2rem\">\
                     前端尚未构建。执行 <code>cd web && npm install && npm run build</code>，\
                     或用 <code>PULSE_WEB_DIR</code> 指向已构建的目录。",
                )
            }),
        );
    }
    // 带 hash 的资源（/assets/xxx-a1b2c3.js）内容一变文件名就变，
    // 所以可以放心地让浏览器永久缓存。不打这个头的话每次导航都要发一次
    // 条件请求 —— 本地没感觉，走 Cloudflare 隧道或跨洲访问时就是肉眼可见的卡顿。
    //
    // index.html 相反：**必须每次都回源校验**，否则发了新版本用户还拿着旧的
    // 入口文件，里面引的是已经不存在的 hash 文件名，页面直接白屏。
    Router::new()
        .nest_service(
            "/assets",
            ServeDir::new(std::path::Path::new(dir).join("assets")),
        )
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        ))
        .fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        ))
}

pub fn router(ctx: Ctx, web_dir: &str) -> Router {
    Router::new()
        .merge(public::routes())
        .merge(admin::routes())
        .merge(billing::routes())
        .merge(notify::routes())
        .merge(visitor::routes())
        .merge(live::routes())
        .merge(agent::routes())
        // 静态文件放最后：API 路由优先匹配
        .merge(static_files(web_dir))
        // 压缩放在最外层，API 的 JSON 和前端产物都能受益。
        // 前端 index.js 近 300 KB，gzip 后 84 KB —— 跨洲或走隧道时差别很明显。
        .layer(CompressionLayer::new())
        .with_state(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdrs(xff: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(v) = xff {
            h.insert("x-forwarded-for", v.parse().unwrap());
        }
        h
    }
    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn direct_connection_ignores_forwarded_header() {
        // 这是默认配置，也是唯一安全的默认：直连时 XFF 完全由客户端控制
        let h = hdrs(Some("1.2.3.4, 5.6.7.8"));
        assert_eq!(client_ip(&h, ip("203.0.113.9"), 0), ip("203.0.113.9"));
    }

    #[test]
    fn one_trusted_proxy_takes_the_rightmost_entry() {
        // 链：[真实客户端, ...]；一层代理时它自己把真实客户端追加到最右
        let h = hdrs(Some("203.0.113.7"));
        assert_eq!(client_ip(&h, ip("10.0.0.1"), 1), ip("203.0.113.7"));
    }

    #[test]
    fn two_trusted_proxies_skip_the_inner_one() {
        let h = hdrs(Some("203.0.113.7, 10.0.0.5"));
        assert_eq!(client_ip(&h, ip("10.0.0.1"), 2), ip("203.0.113.7"));
    }

    #[test]
    fn spoofed_extra_entries_cannot_shift_the_result() {
        // 客户端往 XFF 里塞假 IP：配了 1 层代理时我们只看最右一个，
        // 前面塞多少都影响不到
        let h = hdrs(Some("6.6.6.6, 7.7.7.7, 203.0.113.7"));
        assert_eq!(client_ip(&h, ip("10.0.0.1"), 1), ip("203.0.113.7"));
    }

    #[test]
    fn malformed_or_missing_header_falls_back_to_peer() {
        assert_eq!(client_ip(&hdrs(None), ip("10.0.0.1"), 1), ip("10.0.0.1"));
        assert_eq!(
            client_ip(&hdrs(Some("")), ip("10.0.0.1"), 1),
            ip("10.0.0.1")
        );
        assert_eq!(
            client_ip(&hdrs(Some("not-an-ip")), ip("10.0.0.1"), 1),
            ip("10.0.0.1")
        );
        // 配了 3 层但只有 1 个条目：取不到就回落，不能 panic
        assert_eq!(
            client_ip(&hdrs(Some("1.2.3.4")), ip("10.0.0.1"), 3),
            ip("10.0.0.1")
        );
    }

    #[test]
    fn ipv6_is_supported() {
        let h = hdrs(Some("2001:db8::1"));
        assert_eq!(client_ip(&h, ip("10.0.0.1"), 1), ip("2001:db8::1"));
    }

    #[test]
    fn bearer_extraction() {
        let mut h = HeaderMap::new();
        assert_eq!(bearer(&h), None);
        h.insert("authorization", "Bearer abc123".parse().unwrap());
        assert_eq!(bearer(&h).as_deref(), Some("abc123"));
        h.insert("authorization", "Basic abc123".parse().unwrap());
        assert_eq!(bearer(&h), None, "只接受 Bearer");
        h.insert("authorization", "Bearer    ".parse().unwrap());
        assert_eq!(bearer(&h), None, "空 token 视为没有");
    }
}

#[cfg(test)]
mod error_mapping_tests {
    use super::*;
    use crate::store::Storage;

    /// 回归：重名建分组曾经返回 500「内部错误」。
    ///
    /// 那是**用户输入**造成的错误，后台却只显示一句没头没脑的「内部错误」，
    /// 用户根本不知道该改什么。必须是 409 + 能看懂的话。
    #[tokio::test]
    async fn duplicate_name_becomes_409_not_500() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}/t.db", dir.path().display());
        let store = crate::store::sqlite::SqliteStore::open(&url).await.unwrap();

        let g = crate::store::GroupRow {
            id: 0,
            name: "同名".into(),
            color: None,
            icon: None,
            sort_order: 0,
        };
        store.create_group(&g, 0).await.expect("第一次应当成功");
        let e = store
            .create_group(&g, 0)
            .await
            .expect_err("第二次应当撞唯一约束");

        let api_err = ApiError::db("创建分组", "已有同名分组", e);
        assert_eq!(api_err.status, StatusCode::CONFLICT);
        assert_eq!(api_err.message, "已有同名分组");
        assert_eq!(api_err.code, "CONFLICT");
    }

    /// 反面：不是唯一约束的错误仍然要走 500，且**不能**把数据库原文漏给客户端。
    #[tokio::test]
    async fn other_db_errors_stay_internal_and_leak_nothing() {
        let e = anyhow::anyhow!("no such table: server_billing");
        let api_err = ApiError::db("查询账单", "已有同名的东西", e);
        assert_eq!(api_err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(api_err.message, "内部错误");
        assert!(!api_err.message.contains("server_billing"));
    }
}
