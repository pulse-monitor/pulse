//! 管理接口。全部需要 JWT —— 靠 [`Admin`] 提取器强制。

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::{client_ip, Admin, ApiError, ApiResult, Ctx};
use crate::auth::{self, TokenKind, ACCESS_TTL, REFRESH_TTL};
use crate::install::{self, InstallOptions};
use crate::state::now_unix;
use crate::store::{PingTaskInput, ServerId, ServerPatch};

const REFRESH_COOKIE: &str = "pulse_refresh";

pub fn routes() -> Router<Ctx> {
    Router::new()
        .route("/api/v1/auth/setup", get(setup_status).post(setup))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/refresh", post(refresh))
        .route("/api/v1/auth/logout", post(logout))
        .route(
            "/api/v1/admin/servers",
            get(list_servers).post(create_server),
        )
        .route(
            "/api/v1/admin/servers/{id}",
            get(get_server).put(update_server).delete(delete_server),
        )
        .route("/api/v1/admin/servers/{id}/install", post(install_command))
        .route(
            "/api/v1/admin/servers/{id}/regenerate-token",
            post(regen_token),
        )
        .route("/api/v1/admin/servers/{id}/upgrade", post(trigger_upgrade))
        .route(
            "/api/v1/admin/servers/{id}/runtime-config",
            get(get_runtime_config).put(set_runtime_config),
        )
        .route("/api/v1/admin/servers/reorder", put(reorder))
        .route(
            "/api/v1/admin/ping-tasks",
            get(list_ping_tasks).post(create_ping_task),
        )
        .route(
            "/api/v1/admin/ping-tasks/{id}",
            put(update_ping_task).delete(delete_ping_task),
        )
        .route("/api/v1/admin/health", get(health))
}

// ---------------------------------------------------------------------------
// 认证
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LoginReq {
    username: String,
    password: String,
}

#[derive(Serialize)]
pub struct LoginResp {
    access_token: String,
    expires_in: u64,
}

async fn login(
    State(ctx): State<Ctx>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<LoginReq>,
) -> ApiResult<(CookieJar, Json<LoginResp>)> {
    let ip = client_ip(&headers, peer.ip(), ctx.config.trusted_proxy_hops);
    let now_i = std::time::Instant::now();

    // 限流先于验密码：被限流时连密码都不该去算，否则 argon2 就成了 DoS 放大器
    if let Err(block) = ctx.limiter.check(ip, now_i) {
        warn!(%ip, "登录被限流");
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
            format!(
                "请求过于频繁，请 {} 秒后再试",
                block.retry_after().as_secs()
            ),
        ));
    }

    let user = ctx
        .store
        .get_admin(&req.username)
        .await
        .map_err(|e| ApiError::internal("查询管理员", e))?;

    // 用户不存在时也要走一次同样耗时的哈希校验，否则响应时间会泄露
    // 「这个用户名存在吗」
    let ok = match &user {
        Some(u) => auth::verify_password(&req.password, &u.password_hash),
        None => {
            auth::verify_password(&req.password, DUMMY_HASH);
            false
        }
    };

    if !ok {
        ctx.limiter.record_failure(ip, now_i);
        warn!(%ip, user = %req.username, "登录失败");
        // 不区分「用户不存在」和「密码错误」
        return Err(ApiError::unauthorized("用户名或密码错误"));
    }
    let user = user.expect("ok 为 true 时用户必然存在");
    ctx.limiter.record_success(ip);

    let now = now_unix();
    let out = issue_session(&ctx, jar, &user.username)?;
    let _ = ctx.store.touch_admin_login(user.id, now).await;
    info!(%ip, user = %user.username, "登录成功");
    Ok(out)
}

/// 签发 access + refresh，并把 refresh 放进 cookie。
///
/// 登录和首次初始化共用 —— 初始化完直接就是登录态，不该让用户刚设完
/// 密码又立刻输一遍。
fn issue_session(
    ctx: &Ctx,
    jar: CookieJar,
    username: &str,
) -> Result<(CookieJar, Json<LoginResp>), ApiError> {
    let now = now_unix();
    let (access, _) = ctx
        .jwt
        .issue(username, TokenKind::Access, now)
        .map_err(|e| ApiError::internal("签发 access token", e))?;
    let (refresh, _) = ctx
        .jwt
        .issue(username, TokenKind::Refresh, now)
        .map_err(|e| ApiError::internal("签发 refresh token", e))?;
    Ok((
        jar.add(refresh_cookie(
            refresh,
            ctx.config.panel_url.starts_with("https"),
        )),
        Json(LoginResp {
            access_token: access,
            expires_in: ACCESS_TTL.as_secs(),
        }),
    ))
}

// ---------------------------------------------------------------------------
// 首次初始化
// ---------------------------------------------------------------------------

/// 用户名允许的字符与长度。留得比较紧 —— 它会进日志和 JWT 的 sub。
const USERNAME_MIN: usize = 3;
const USERNAME_MAX: usize = 32;
/// 与 `PULSE_ADMIN_PASSWORD` 的门槛保持一致，避免两条路径的规则不同。
const PASSWORD_MIN: usize = 8;

#[derive(Serialize)]
pub struct SetupStatus {
    /// true = 还没有任何管理员，前端该显示「创建管理员」而不是「登录」
    needed: bool,
}

#[derive(Deserialize)]
pub struct SetupReq {
    username: String,
    password: String,
}

/// 面板还需不需要初始化。
///
/// 公开可读，且**只回一个布尔**：它本来就能从「登录页长什么样」推出来，
/// 藏着没有意义，反而会让前端得靠猜。
async fn setup_status(State(ctx): State<Ctx>) -> ApiResult<Json<SetupStatus>> {
    let n = ctx
        .store
        .count_admins()
        .await
        .map_err(|e| ApiError::internal("查询管理员数量", e))?;
    Ok(Json(SetupStatus { needed: n == 0 }))
}

/// 创建第一个管理员，并直接进入登录态。
///
/// **安全边界**：这个接口在「一个管理员都没有」时对所有人开放，也就是说
/// 从面板起来到有人完成初始化之间，谁先访问谁就是管理员。这是产品上要的
/// 交互（装完点开就能用，不必去日志里翻密码），代价必须说清楚：
///
/// - 判空与写入是同一条 SQL（见 `create_first_admin`），不存在两个人都建成的情况；
/// - 建成之后这个接口永久失效，返回 409；
/// - 谁抢到了会以 warn 级别记下 IP 和用户名，事后能查；
/// - 想完全关掉这个窗口的，可以在启动时给 `PULSE_ADMIN_PASSWORD`
///   预先建好管理员，那样这里从一开始就是 409。
///
/// 所以安装脚本会把面板地址直接打出来，提示**立刻**完成初始化。
async fn setup(
    State(ctx): State<Ctx>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<SetupReq>,
) -> ApiResult<(CookieJar, Json<LoginResp>)> {
    let ip = client_ip(&headers, peer.ip(), ctx.config.trusted_proxy_hops);

    // 顺序是有讲究的：**先挡掉不花钱的情况，再限流**。
    //
    // 限流器保护的是 argon2 —— 那才是能被放大成 DoS 的东西。而「已经初始化过」
    // 和「用户名不合法」都是常数开销，让它们去消耗额度的话，初始化完的人
    // 紧接着就会被自己刚才那几次请求挡在登录门外（实测过）。
    if ctx
        .store
        .count_admins()
        .await
        .map_err(|e| ApiError::internal("查询管理员数量", e))?
        > 0
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "ALREADY_INITIALIZED",
            "面板已经初始化过了，请直接登录",
        ));
    }

    let username = req.username.trim();
    if !(USERNAME_MIN..=USERNAME_MAX).contains(&username.chars().count()) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "BAD_USERNAME",
            format!("用户名需要 {USERNAME_MIN}-{USERNAME_MAX} 个字符"),
        ));
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "BAD_USERNAME",
            "用户名只能包含字母、数字、下划线和连字符",
        ));
    }
    // 按**字符数**而不是字节数算，否则中文密码会被高估长度
    if req.password.chars().count() < PASSWORD_MIN {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "BAD_PASSWORD",
            format!("密码至少需要 {PASSWORD_MIN} 个字符"),
        ));
    }

    // 到这里才是真正花钱的一段：argon2 + 写库
    if let Err(block) = ctx.limiter.check(ip, std::time::Instant::now()) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
            format!(
                "请求过于频繁，请 {} 秒后再试",
                block.retry_after().as_secs()
            ),
        ));
    }

    let hash =
        auth::hash_password(&req.password).map_err(|e| ApiError::internal("计算密码哈希", e))?;

    let created = ctx
        .store
        .create_first_admin(username, &hash, now_unix())
        .await
        .map_err(|e| ApiError::internal("创建管理员", e))?;

    let Some(id) = created else {
        warn!(%ip, "初始化被拒绝：管理员已存在");
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "ALREADY_INITIALIZED",
            "面板已经初始化过了，请直接登录",
        ));
    };

    // warn 而不是 info：这条是事后追责的锚点，不该淹在 info 里
    warn!(%ip, user = %username, "面板已初始化，管理员由该 IP 创建");

    let out = issue_session(&ctx, jar, username)?;
    let _ = ctx.store.touch_admin_login(id, now_unix()).await;
    Ok(out)
}

/// 用户不存在时用来消耗等量时间的假哈希。参数与真实哈希一致。
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$YWFhYWFhYWFhYWFhYWFhYQ$\
                          bWFkZXVwaGFzaHZhbHVlZm9ydGltaW5n";

fn refresh_cookie(value: String, secure: bool) -> Cookie<'static> {
    Cookie::build((REFRESH_COOKIE, value))
        .http_only(true) // JS 拿不到，XSS 也偷不走
        .secure(secure)
        .same_site(SameSite::Strict) // 挡住 CSRF
        .path("/api/v1/auth")
        .max_age(cookie_duration(REFRESH_TTL))
        .build()
}

/// `std::time::Duration` → cookie 用的 `time::Duration`。
fn cookie_duration(d: std::time::Duration) -> time::Duration {
    time::Duration::seconds(d.as_secs() as i64)
}

async fn refresh(
    State(ctx): State<Ctx>,
    jar: CookieJar,
) -> ApiResult<(CookieJar, Json<LoginResp>)> {
    let token = jar
        .get(REFRESH_COOKIE)
        .map(|c| c.value().to_string())
        .ok_or_else(|| ApiError::unauthorized("缺少 refresh cookie"))?;

    let claims = ctx
        .jwt
        .verify(&token, TokenKind::Refresh)
        .map_err(|e| ApiError::unauthorized(format!("refresh 无效: {e}")))?;

    // 吊销表用 jti 的哈希做键
    let jti_hash = auth::token_hash(&claims.jti);
    if ctx
        .store
        .is_revoked(&jti_hash)
        .await
        .map_err(|e| ApiError::internal("查询吊销表", e))?
    {
        return Err(ApiError::unauthorized("该会话已退出登录"));
    }

    let now = now_unix();
    let (access, _) = ctx
        .jwt
        .issue(&claims.sub, TokenKind::Access, now)
        .map_err(|e| ApiError::internal("签发 access token", e))?;

    // 轮换 refresh：旧的立刻作废。这样 refresh 被偷走后，
    // 真正的用户一续期就会让攻击者手里的那个失效
    let _ = ctx.store.revoke_token(&jti_hash, claims.exp).await;
    let (new_refresh, _) = ctx
        .jwt
        .issue(&claims.sub, TokenKind::Refresh, now)
        .map_err(|e| ApiError::internal("签发 refresh token", e))?;

    Ok((
        jar.add(refresh_cookie(
            new_refresh,
            ctx.config.panel_url.starts_with("https"),
        )),
        Json(LoginResp {
            access_token: access,
            expires_in: ACCESS_TTL.as_secs(),
        }),
    ))
}

async fn logout(State(ctx): State<Ctx>, jar: CookieJar) -> ApiResult<(CookieJar, StatusCode)> {
    if let Some(c) = jar.get(REFRESH_COOKIE) {
        if let Ok(claims) = ctx.jwt.verify(c.value(), TokenKind::Refresh) {
            let _ = ctx
                .store
                .revoke_token(&auth::token_hash(&claims.jti), claims.exp)
                .await;
        }
    }
    Ok((
        jar.remove(Cookie::from(REFRESH_COOKIE)),
        StatusCode::NO_CONTENT,
    ))
}

// ---------------------------------------------------------------------------
// 服务器管理
// ---------------------------------------------------------------------------

async fn list_servers(_: Admin, State(ctx): State<Ctx>) -> ApiResult<impl IntoResponse> {
    let servers = ctx
        .store
        .list_servers()
        .await
        .map_err(|e| ApiError::internal("列出服务器", e))?;
    Ok(Json(servers))
}

async fn get_server(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<ServerId>,
) -> ApiResult<impl IntoResponse> {
    ctx.store
        .get_server(id)
        .await
        .map_err(|e| ApiError::internal("查询服务器", e))?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("服务器不存在"))
}

#[derive(Deserialize)]
pub struct CreateServerReq {
    name: String,
}

#[derive(Serialize)]
pub struct CreatedServer {
    #[serde(flatten)]
    server: crate::store::ServerRecord,
    /// **明文 token 只在这里出现这一次**，之后数据库里只有它的 SHA-256
    token: String,
    note: &'static str,
}

async fn create_server(
    _: Admin,
    State(ctx): State<Ctx>,
    Json(req): Json<CreateServerReq>,
) -> ApiResult<impl IntoResponse> {
    let name = req.name.trim();
    if name.is_empty() || name.len() > 128 {
        return Err(ApiError::bad_request("名称需为 1..=128 个字符"));
    }
    let token = auth::generate_token().map_err(|e| ApiError::internal("生成 token", e))?;
    let server = ctx
        .store
        .create_server(name, &auth::token_hash(&token), now_unix())
        .await
        .map_err(|e| ApiError::db("创建服务器", "已有同名服务器", e))?;

    info!(id = server.id, %name, "创建服务器");
    Ok((
        StatusCode::CREATED,
        Json(CreatedServer {
            server,
            token,
            note: "token 只显示这一次。数据库里只存它的 SHA-256，丢了只能重新生成。",
        }),
    ))
}

async fn update_server(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<ServerId>,
    Json(patch): Json<ServerPatch>,
) -> ApiResult<StatusCode> {
    let changed = ctx
        .store
        .update_server(id, &patch)
        .await
        .map_err(|e| ApiError::db("更新服务器", "已有同名服务器", e))?;
    if !changed {
        return Err(ApiError::not_found(
            "服务器不存在，或请求里没有任何要改的字段",
        ));
    }
    // 立刻同步到内存态。不同步的话，改完名字要等 agent 下次重连才生效 ——
    // 一台连接稳定的机器可能几个月都不重连，用户会以为改名根本没保存。
    if let Ok(Some(sv)) = ctx.store.get_server(id).await {
        ctx.state.set_meta(&sv.uuid, (&sv).into());
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_server(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<ServerId>,
) -> ApiResult<StatusCode> {
    // 先取 uuid：删完就查不到了，而内存里的条目要靠它来清
    let uuid = ctx
        .store
        .get_server(id)
        .await
        .map_err(|e| ApiError::internal("查询服务器", e))?
        .map(|s| s.uuid);

    // 外键 CASCADE：指标与延迟数据一并清掉
    if ctx
        .store
        .delete_server(id)
        .await
        .map_err(|e| ApiError::internal("删除服务器", e))?
    {
        if let Some(u) = uuid {
            ctx.state.forget(&u);
        }
        info!(id, "删除服务器（含其全部历史数据）");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("服务器不存在"))
    }
}

#[derive(Serialize)]
pub struct RegenResp {
    token: String,
    note: &'static str,
}

async fn regen_token(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<ServerId>,
) -> ApiResult<impl IntoResponse> {
    let token = auth::generate_token().map_err(|e| ApiError::internal("生成 token", e))?;
    if !ctx
        .store
        .set_server_token(id, &auth::token_hash(&token))
        .await
        .map_err(|e| ApiError::internal("更新 token", e))?
    {
        return Err(ApiError::not_found("服务器不存在"));
    }
    warn!(id, "已重置 agent token，旧凭据立即失效");
    Ok(Json(RegenResp {
        token,
        note: "旧 token 已立即失效，该机器的 agent 需要用新 token 重装或改配置。",
    }))
}

#[derive(Deserialize)]
pub struct ReorderReq {
    order: Vec<ServerId>,
}

async fn reorder(
    _: Admin,
    State(ctx): State<Ctx>,
    Json(req): Json<ReorderReq>,
) -> ApiResult<StatusCode> {
    let pairs: Vec<_> = req
        .order
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, i as i64))
        .collect();
    ctx.store
        .reorder_servers(&pairs)
        .await
        .map_err(|e| ApiError::internal("排序", e))?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// 安装命令
// ---------------------------------------------------------------------------

async fn install_command(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<ServerId>,
    Json(opts): Json<InstallOptions>,
) -> ApiResult<impl IntoResponse> {
    let opts = opts.sanitize();
    if ctx
        .store
        .get_server(id)
        .await
        .map_err(|e| ApiError::internal("查询服务器", e))?
        .is_none()
    {
        return Err(ApiError::not_found("服务器不存在"));
    }

    // 生成安装命令的同时**重置 token** —— 因为数据库里只有哈希，
    // 拿不回原来的明文。这也顺带保证了「重新拿安装命令」= 「换一把新钥匙」。
    let token = auth::generate_token().map_err(|e| ApiError::internal("生成 token", e))?;
    ctx.store
        .set_server_token(id, &auth::token_hash(&token))
        .await
        .map_err(|e| ApiError::internal("更新 token", e))?;

    // 运行期选项同时写库：这样后台以后改这些值时，改完 ≤2 秒生效，不用碰机器
    let cfg = pulse_proto::RuntimeConfig {
        interval_s: opts.interval_s,
        net_include: opts.net_include.clone(),
        net_exclude: if opts.net_exclude.is_empty() {
            pulse_proto::default_net_exclude()
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            opts.net_exclude.clone()
        },
        gpu_enabled: opts.enable_gpu,
        ..Default::default()
    };
    ctx.store
        .set_runtime_config(id, &cfg, now_unix())
        .await
        .map_err(|e| ApiError::internal("保存运行期配置", e))?;

    Ok(Json(install::render(&ctx.config.panel_url, &token, &opts)))
}

// ---------------------------------------------------------------------------
// 运行期配置
// ---------------------------------------------------------------------------

async fn get_runtime_config(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<ServerId>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        ctx.store
            .get_runtime_config(id)
            .await
            .map_err(|e| ApiError::internal("读取运行期配置", e))?,
    ))
}

#[derive(Deserialize)]
pub struct UpgradeReq {
    /// 目标版本，形如 `0.4.0`
    version: String,
}

/// 让一台 agent 升级。
///
/// **面板只发版本号**：下载地址来自 agent 本地配置，agent 侧还会验签、
/// 校验摘要、拒绝降级、并在试用期内没连上时自动回滚。
/// 即使面板被完全攻陷，这个接口也变不成远程执行 —— 这是 R18 的核心。
async fn trigger_upgrade(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<ServerId>,
    Json(req): Json<UpgradeReq>,
) -> ApiResult<impl IntoResponse> {
    let v = req.version.trim();
    // 这里也校一次版本号格式。agent 侧一定会再校一次 ——
    // 但把畸形输入挡在最外层，日志里看到的就是「谁发了什么」而不是一堆 agent 的报错
    if !is_plain_semver(v) {
        return Err(ApiError::bad_request("版本号必须形如 0.4.0"));
    }
    let server = ctx
        .store
        .get_server(id)
        .await
        .map_err(|e| ApiError::internal("查询服务器", e))?
        .ok_or_else(|| ApiError::not_found("服务器不存在"))?;

    if !ctx.state.push_upgrade(&server.uuid, v.to_string()) {
        return Err(ApiError::conflict("这台机器当前不在线，无法下发升级指令"));
    }
    info!(id, version = v, "已下发升级指令");
    Ok(Json(serde_json::json!({
        "delivered": true,
        "note": "已下发。agent 会自行校验签名与摘要，失败则保持当前版本；\
                 新版本在 60 秒内没能连回面板会自动回滚。"
    })))
}

/// `a.b.c`，三段纯数字。不接受 `v` 前缀、预发布后缀、路径分隔符。
fn is_plain_semver(s: &str) -> bool {
    let mut parts = 0;
    for p in s.split('.') {
        parts += 1;
        if p.is_empty() || p.len() > 6 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    parts == 3
}

async fn set_runtime_config(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<ServerId>,
    Json(cfg): Json<pulse_proto::RuntimeConfig>,
) -> ApiResult<StatusCode> {
    let cfg = cfg.sanitize();
    let server = ctx
        .store
        .get_server(id)
        .await
        .map_err(|e| ApiError::internal("查询服务器", e))?
        .ok_or_else(|| ApiError::not_found("服务器不存在"))?;

    ctx.store
        .set_runtime_config(id, &cfg, now_unix())
        .await
        .map_err(|e| ApiError::internal("保存运行期配置", e))?;

    // 立刻推给在线的 agent —— 这就是「后台改完 ≤2 秒生效」的实现
    let delivered = ctx.state.push_config(&server.uuid, cfg);
    info!(id, delivered, "更新运行期配置");
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// 延迟监控任务
// ---------------------------------------------------------------------------

async fn list_ping_tasks(_: Admin, State(ctx): State<Ctx>) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        ctx.store
            .list_ping_tasks()
            .await
            .map_err(|e| ApiError::internal("列出探测任务", e))?,
    ))
}

/// 校验任务定义。
///
/// 这些值会变成**真实的出网连接**：一条配置就能让所有机器同时对某个地址
/// 发起洪水，所以数量与频率必须在入口就卡死（agent 侧还会再夹一次）。
fn validate(t: &PingTaskInput) -> ApiResult<()> {
    if t.name.trim().is_empty() || t.name.len() > 64 {
        return Err(ApiError::bad_request("名称需为 1..=64 个字符"));
    }
    if !matches!(t.kind.as_str(), "tcp" | "icmp" | "http") {
        return Err(ApiError::bad_request("kind 只能是 tcp / icmp / http"));
    }
    if t.host.trim().is_empty() || t.host.len() > 255 {
        return Err(ApiError::bad_request("host 需为 1..=255 个字符"));
    }
    if t.kind == "tcp" && t.port.is_none() {
        return Err(ApiError::bad_request("tcp 任务必须指定端口"));
    }
    if t.kind == "http" && !(t.host.starts_with("http://") || t.host.starts_with("https://")) {
        return Err(ApiError::bad_request("http 任务的 host 必须是完整 URL"));
    }
    if !(pulse_proto::PingTaskSpec::MIN_INTERVAL_S..=3600).contains(&t.interval_s) {
        return Err(ApiError::bad_request(format!(
            "间隔需在 {}..=3600 秒之间 —— 太密会对目标造成压力",
            pulse_proto::PingTaskSpec::MIN_INTERVAL_S
        )));
    }
    if !(1..=pulse_proto::PingTaskSpec::MAX_PACKETS).contains(&t.packets) {
        return Err(ApiError::bad_request("每轮包数需在 1..=10 之间"));
    }
    Ok(())
}

/// 任务变更后给所有在线机器重推一遍。
///
/// 挨个查作用范围而不是算增量：这是低频的后台操作，200 台也就 200 次
/// 索引查询；而增量逻辑一旦算错就会留下"幽灵任务"，代价高得多。
async fn repush_tasks(ctx: &Ctx) -> usize {
    let mut n = 0;
    for (uuid, id) in ctx.state.online_servers() {
        if let Ok(tasks) = ctx.store.ping_tasks_for_server(id).await {
            if ctx.state.push_tasks(&uuid, tasks) {
                n += 1;
            }
        }
    }
    n
}

async fn create_ping_task(
    _: Admin,
    State(ctx): State<Ctx>,
    Json(t): Json<PingTaskInput>,
) -> ApiResult<impl IntoResponse> {
    validate(&t)?;
    let row = ctx
        .store
        .create_ping_task(&t, now_unix())
        .await
        .map_err(|e| ApiError::db("创建探测任务", "已有同名探测任务", e))?;
    let pushed = repush_tasks(&ctx).await;
    info!(id = row.id, name = %row.name, pushed, "创建探测任务");
    Ok((StatusCode::CREATED, Json(row)))
}

async fn update_ping_task(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<i64>,
    Json(t): Json<PingTaskInput>,
) -> ApiResult<StatusCode> {
    validate(&t)?;
    if !ctx
        .store
        .update_ping_task(id, &t)
        .await
        .map_err(|e| ApiError::db("更新探测任务", "已有同名探测任务", e))?
    {
        return Err(ApiError::not_found("任务不存在"));
    }
    let pushed = repush_tasks(&ctx).await;
    info!(id, pushed, "更新探测任务");
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_ping_task(
    _: Admin,
    State(ctx): State<Ctx>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    if !ctx
        .store
        .delete_ping_task(id)
        .await
        .map_err(|e| ApiError::internal("删除探测任务", e))?
    {
        return Err(ApiError::not_found("任务不存在"));
    }
    // 历史结果不删：任务没了，但已经采到的数据还有价值，
    // 它们会按正常保留期自然过期
    let pushed = repush_tasks(&ctx).await;
    info!(id, pushed, "删除探测任务（保留历史数据）");
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// 运维
// ---------------------------------------------------------------------------

async fn health(_: Admin, State(ctx): State<Ctx>) -> ApiResult<impl IntoResponse> {
    // 这个接口是 那套容量预算在生产环境的持续验证点
    let stats = ctx.store.stats().await.ok();
    let s = ctx.state.summary();
    Ok(Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "milestone": "M3",
        "ws": { "agents": s.total, "online": s.online },
        "db": stats,
    })))
}

#[cfg(test)]
mod upgrade_tests {
    use super::is_plain_semver;

    #[test]
    fn accepts_plain_three_part_versions() {
        for ok in ["0.0.1", "1.2.3", "10.20.30", "0.0.0"] {
            assert!(is_plain_semver(ok), "应当接受 {ok}");
        }
    }

    /// 版本号会被拼进下载 URL，所以路径穿越、前缀、后缀一律拒绝。
    /// agent 侧还会再校一次，但畸形输入应当在最外层就被挡下来。
    #[test]
    fn rejects_anything_that_could_escape_a_url_path() {
        for bad in [
            "",
            "1.0",
            "1.0.0.0",
            "v1.0.0",
            "1.0.0-rc1",
            "1.0.0+build",
            "../../etc/passwd",
            "1.0.0/../../x",
            "1.0.0 ",
            " 1.0.0",
            "1.0.x",
            "1.-1.0",
            "1234567.0.0",
        ] {
            assert!(!is_plain_semver(bad), "不该接受 {bad:?}");
        }
    }
}
