//! Pulse 服务端。
//!
//! M0 骨架 → M1 分层存储 → M2 全量指标 → **M3 认证 / 服务器管理 / 一键安装**。
//! 仍未做：分组、账单、延迟监控、通知。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

use pulse_server::api::{self, Ctx, PublicMode, ServerConfig};
use pulse_server::auth::{self, Jwt, LoginLimiter};
use pulse_server::state::{now_unix, AppState};
use pulse_server::store::{sqlite::SqliteStore, RetentionPolicy, Storage};
use pulse_server::tasks;
use pulse_server::tls;

const DEFAULT_BIND: &str = "127.0.0.1:25774";
const DEFAULT_DB: &str = "sqlite://data/pulse.db";
const DEFAULT_ADMIN: &str = "admin";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PULSE_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    // 必须显式安装 rustls 的 crypto provider，否则第一次出站 HTTPS
    // （拉汇率）会在 rustls 内部 panic。agent 那边踩过同样的坑，
    // 现象是「本地全走明文所以一直没暴露」——
    if rustls::crypto::ring::default_provider()
        .install_default()
        .is_err()
    {
        tracing::debug!("crypto provider 已存在");
    }

    let db_url = env("PULSE_DATABASE_URL", DEFAULT_DB);
    let data_dir = db_url
        .strip_prefix("sqlite://")
        .and_then(|p| PathBuf::from(p).parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("data"));
    // create_if_missing 只建文件不建目录，父目录得自己保证
    std::fs::create_dir_all(&data_dir).ok();

    let store: Arc<dyn Storage> = Arc::new(
        SqliteStore::open(&db_url)
            .await
            .with_context(|| format!("打开数据库失败: {db_url}"))?,
    );
    info!(%db_url, "存储已就绪");

    let secret =
        auth::load_or_create_secret(&data_dir.join("secret.key")).context("加载服务端密钥失败")?;

    // 面板时区。账单周期是「你和商家的约定」，用统一时区可解释；
    // 机器时区五花八门会让同一天的重置发生在不同时刻。
    let panel_tz: chrono_tz::Tz = env("PULSE_TIMEZONE", "Asia/Shanghai")
        .parse()
        .unwrap_or(chrono_tz::Asia::Shanghai);
    let rate_base = env("PULSE_RATE_BASE_URL", "https://rate.jinqians.com");

    let config = Arc::new(ServerConfig {
        panel_url: env(
            "PULSE_PUBLIC_URL",
            &format!("http://{}", env("PULSE_BIND", DEFAULT_BIND)),
        ),
        trusted_proxy_hops: env("PULSE_TRUSTED_PROXY_HOPS", "0").parse().unwrap_or(0),
        public_mode: match env("PULSE_PUBLIC_MODE", "public").as_str() {
            "private" => PublicMode::Private,
            _ => PublicMode::Public,
        },
        rate_base_url: rate_base.clone(),
        display_currency: env("PULSE_DISPLAY_CURRENCY", "CNY").to_ascii_uppercase(),
        timezone: panel_tz,
        visitor_badge: env("PULSE_VISITOR_BADGE", "true") != "false",
    });
    // 放在 config 之后：没有管理员时要把面板地址打进日志，得先知道地址
    ensure_admin(store.as_ref(), &config.panel_url).await?;

    if config.trusted_proxy_hops == 0 {
        info!("未配置可信代理层数，将忽略 X-Forwarded-For（直连部署的正确默认值）");
    } else {
        info!(hops = config.trusted_proxy_hops, "已配置可信代理层数");
    }

    let state = AppState::new();
    let limiter = Arc::new(LoginLimiter::new());
    tasks::spawn_all(state.clone(), store.clone(), RetentionPolicy::default());
    tasks::spawn_auth_maintenance(store.clone(), limiter.clone());
    tasks::spawn_business(
        state.clone(),
        store.clone(),
        panel_tz,
        rate_base.clone(),
        config.display_currency.clone(),
    );
    tasks::spawn_notify(
        state.clone(),
        store.clone(),
        secret.clone(),
        config.panel_url.clone(),
        panel_tz,
    );
    info!(tz = %panel_tz, "面板时区");

    // GeoIP：先用磁盘缓存把表建起来（**离线也能用**），再由后台任务决定是否刷新
    let data_dir = std::path::PathBuf::from(env("PULSE_DATA_DIR", "data"));
    let geoip: tasks::geoip::SharedTable = Arc::new(arc_swap::ArcSwap::from_pointee(
        tasks::geoip::load_cached(&data_dir),
    ));
    tasks::geoip::spawn(data_dir, geoip.clone());

    let ctx = Ctx {
        state,
        store,
        jwt: Jwt::new(&secret),
        limiter,
        config: config.clone(),
        secret: Arc::new(secret.clone()),
        geoip,
    };

    let web_dir = env("PULSE_WEB_DIR", "web/dist");
    let app = api::router(ctx, &web_dir)
        // 前后端分离部署时前端在另一个域名上，所以需要 CORS。
        // M3 仍用 permissive 便于本地调试；生产必须换成显式白名单
        //（把面板自己的域名列进 allow_origin）。
        .layer(CorsLayer::permissive());

    let bind = env("PULSE_BIND", DEFAULT_BIND);
    let addr: SocketAddr = bind
        .parse()
        .with_context(|| format!("PULSE_BIND 不是合法的监听地址：{bind}"))?;

    let tls = tls::TlsPaths::from_env(
        std::env::var("PULSE_TLS_CERT").ok(),
        std::env::var("PULSE_TLS_KEY").ok(),
    )?;

    // 明文 HTTP 发到公网上时警告 —— **探针的 token 就放在 WebSocket 握手里**，
    // 明文链路上任何一跳都能拿到它，拿到就能冒充这台机器上报。
    // 只提醒不阻止：内网、或者前面已经有反代终止 HTTPS 的，都是合理部署。
    if tls.is_none() && tls::insecure_public_url(&config.panel_url) {
        warn!(
            panel_url = %config.panel_url,
            "面板对外地址是明文 http://，探针 token 会以明文经过网络。\n         \
             要么配 PULSE_TLS_CERT / PULSE_TLS_KEY 让面板自己跑 HTTPS，\n         \
             要么放到反代后面并把 PULSE_PUBLIC_URL 设成 https:// 的域名\n         \
             （反代方式还要设 PULSE_TRUSTED_PROXY_HOPS，否则拿到的客户端 IP 是反代的）"
        );
    }

    // into_make_service_with_connect_info：登录限流与访客标签需要对端地址
    let svc = app.into_make_service_with_connect_info::<SocketAddr>();

    match tls {
        Some(paths) => {
            let cfg = paths.load().await?;
            info!(%bind, panel_url = %config.panel_url, %web_dir, tls = true, "pulse-server 启动");
            // axum-server 自带优雅关闭句柄，和 axum::serve 的写法不一样
            let handle = axum_server::Handle::new();
            let h = handle.clone();
            tokio::spawn(async move {
                shutdown_signal().await;
                // 给在途请求 10 秒收尾。WebSocket 是长连接，等它们自然断没有意义
                h.graceful_shutdown(Some(std::time::Duration::from_secs(10)));
            });
            axum_server::bind_rustls(addr, cfg)
                .handle(handle)
                .serve(svc)
                .await
                .context("HTTPS 服务异常退出")?;
        }
        None => {
            let listener = tokio::net::TcpListener::bind(&bind)
                .await
                .with_context(|| format!("无法监听 {bind}"))?;
            info!(%bind, panel_url = %config.panel_url, %web_dir, tls = false, "pulse-server 启动");
            axum::serve(listener, svc)
                .with_graceful_shutdown(shutdown_signal())
                .await
                .context("HTTP 服务异常退出")?;
        }
    }

    info!("已优雅关闭");
    Ok(())
}

fn env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// 首次启动时的管理员引导。
///
/// 默认**什么都不建** —— 管理员由用户打开面板、在初始化页面自行设定
/// 用户名和密码（`POST /api/v1/auth/setup`）。这样安装时不必带参数，
/// 也不用去日志里翻一次性密码。
///
/// 代价是：从面板起来到有人完成初始化之间，谁先访问谁就是管理员。
/// 无人值守安装（Docker、批量部署）如果不能接受这个窗口，就设
/// `PULSE_ADMIN_PASSWORD`（可配 `PULSE_ADMIN_USERNAME`，默认 admin），
/// 这里会先把管理员建好，初始化接口从一开始就是关的。
async fn ensure_admin(store: &dyn Storage, panel_url: &str) -> Result<()> {
    if store.count_admins().await.context("查询管理员数量")? > 0 {
        return Ok(());
    }

    let Ok(password) = std::env::var("PULSE_ADMIN_PASSWORD") else {
        warn!("──────────────────────────────────────────────────────────");
        warn!("  面板尚未初始化。请立即打开下面的地址设置管理员账号：");
        warn!("    {panel_url}");
        warn!("  在设置完成前，任何能访问该地址的人都可以抢先创建管理员。");
        warn!("──────────────────────────────────────────────────────────");
        return Ok(());
    };

    if password.chars().count() < 8 {
        anyhow::bail!("PULSE_ADMIN_PASSWORD 至少需要 8 个字符");
    }
    let username = env("PULSE_ADMIN_USERNAME", DEFAULT_ADMIN);
    let hash = auth::hash_password(&password)?;
    store
        .create_admin(&username, &hash, now_unix())
        .await
        .context("创建管理员失败")?;
    info!("已用 PULSE_ADMIN_PASSWORD 创建管理员 {username}");
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    info!("收到 Ctrl-C，开始关闭");
}
