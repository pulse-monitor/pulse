//! Pulse 探针。
//!
//! 采集本机指标，通过 WebSocket 报给 server，断线按退避重连。
//! 完整设计
//!
//! 安全红线：
//! **不监听端口、不执行外部命令、不接受远程指令。**
//! server 能下发的只有受限枚举配置，agent 侧还会再校验一次。

mod backoff;
mod collect;
mod prober;
mod update;

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use pulse_proto::{
    AgentMsg, Capabilities, Hello, RuntimeConfig, ServerMsg, PROTO_VERSION, WS_SUBPROTOCOL,
};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use crate::backoff::Backoff;
use crate::collect::{Collector, Facts, Platform};
use crate::prober::Prober;

const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

struct Config {
    /// 形如 `ws://127.0.0.1:25774` 或 `wss://panel.example.com`
    server: String,
    token: String,
    /// 自更新的下载源。**只从本地读，server 影响不了它** ——
    /// 这是 R18 的第 1 道防线。
    update_base: String,
    /// 关掉后 agent 只在 Hello 里报版本号，面板显示「可升级」，由用户手动升
    auto_update: bool,
    /// 额外信任的 CA 证书（PEM）。面板用私有 CA 或自签证书时需要。
    ///
    /// **是「额外」不是「替换」**：内置的公共 CA 列表照旧生效，
    /// 这里只是往信任集里加。所以配了它也不会让别的连接变得更宽松。
    ca_cert: Option<String>,
}

impl Config {
    fn from_env() -> Result<Self> {
        // Token 只从环境变量读，**不接受命令行参数** —— 命令行对同机任何用户
        // 都可以通过 `ps` 看到。
        let token = std::env::var("PULSE_TOKEN").context(
            "缺少 PULSE_TOKEN。token 只能通过环境变量或配置文件提供，不接受命令行参数（ps 可见）",
        )?;
        if token.trim().is_empty() {
            bail!("PULSE_TOKEN 为空");
        }
        let server =
            std::env::var("PULSE_SERVER").unwrap_or_else(|_| "ws://127.0.0.1:25774".to_string());
        let update_base = std::env::var("PULSE_UPDATE_BASE").unwrap_or_default();
        // 默认开启，安装脚本的 --disable-auto-update 会把它设成 0
        let auto_update = !matches!(
            std::env::var("PULSE_AUTO_UPDATE").as_deref(),
            Ok("0") | Ok("false") | Ok("no")
        );
        // 私有 CA。留空等同没配 —— systemd 的 EnvironmentFile 里写
        // `PULSE_CA_CERT=` 会得到空串
        let ca_cert = std::env::var("PULSE_CA_CERT")
            .ok()
            .filter(|s| !s.trim().is_empty());

        Ok(Self {
            server,
            token,
            update_base,
            auto_update,
            ca_cert,
        })
    }

    fn ws_url(&self) -> String {
        format!("{}/api/v1/agent/ws", self.server.trim_end_matches('/'))
    }

    /// 自更新是否真的可用。四个条件缺一不可，**如实上报**给面板。
    fn self_update_ready(&self) -> bool {
        self.auto_update && !self.update_base.trim().is_empty() && update::available()
    }
}

pub fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn hello_from(facts: Facts) -> Hello {
    Hello {
        proto_version: PROTO_VERSION,
        agent_version: AGENT_VERSION.to_string(),
        hostname: facts.hostname,
        os: facts.os,
        arch: facts.arch,
        cpu_cores: facts.cpu_cores,
        boot_at: facts.boot_at,
        kernel: facts.kernel,
        cpu_model: facts.cpu_model,
        virtualization: facts.virtualization,
        mem_total: facts.mem_total,
        swap_total: facts.swap_total,
        disk_total: facts.disk_total,
        interfaces: facts.interfaces,
        capabilities: facts.capabilities,
    }
}

/// 把能力声明打进日志，方便用户一眼看出这台机器缺什么。
fn log_capabilities(c: &Capabilities) {
    let missing: Vec<&str> = [
        (!c.temperature, "温度"),
        (!c.tcp_conn_count, "TCP 连接数"),
        (!c.proc_count, "进程数(hidepid?)"),
        (!c.load_average, "负载"),
        (!c.icmp_unprivileged, "非特权 ICMP(延迟监控回落 TCP)"),
    ]
    .into_iter()
    .filter_map(|(missing, name)| missing.then_some(name))
    .collect();

    if missing.is_empty() {
        info!("本机全部指标均可采集");
    } else {
        // 这不是错误：拿不到就如实上报，前端会隐藏对应字段
        info!(不可用 = ?missing, "部分指标在本机不可用，已如实声明");
    }
    if c.cgroup_limited {
        warn!("检测到容器环境且未挂 lxcfs：内存/CPU 规格已改用 cgroup 限额，而非 /proc 里的宿主机数值");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PULSE_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    // 必须显式安装 rustls 的 crypto provider。
    //
    // 不装的话，第一次 wss:// 连接会在 rustls 内部 **panic**
    // （"no process-level CryptoProvider available"），而 panic=abort
    // 意味着 agent 当场死掉。这个 bug 从 M0 一直潜伏到 M4 才被发现 ——
    // 因为本地测试全走 ws://，而生产全走 wss://。
    if rustls::crypto::ring::default_provider()
        .install_default()
        .is_err()
    {
        // 已经装过（不该发生，但装两次不是错误）
        debug!("crypto provider 已存在");
    }

    let cfg = Config::from_env()?;
    info!(server = %cfg.server, version = AGENT_VERSION, "pulse-agent 启动");

    let mut collector = Platform::new();
    let mut backoff = Backoff::new();
    // 上一次更新留下的试用状态。连上面板就提交，试用期内没连上就回滚。
    let pending = update::on_boot(AGENT_VERSION);
    let committed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // 试用期的看门狗**必须是独立任务**。
    //
    // 一开始我把它写成套在会话外面的 select! —— 结果连不上面板时，
    // 指数退避的 sleep 发生在 select 之外，退避涨到几十秒后进程就再也
    // 走不到那个检查点，60 秒的试用期形同虚设（e2e 实测：75 秒没回滚）。
    // 独立任务不受主循环在做什么影响。
    if let Some(p) = pending.clone() {
        let committed = committed.clone();
        tokio::spawn(async move {
            tokio::time::sleep(update::trial_remaining(&p, now_unix())).await;
            if !committed.load(std::sync::atomic::Ordering::SeqCst) {
                update::rollback_and_exit(&p);
            }
        });
    }

    loop {
        let result = run_session(&cfg, &mut collector, pending.as_ref(), &committed).await;

        match result {
            Ok(SessionEnd::Upgraded) => {
                info!("新二进制已就位，退出等待服务管理器重启");
                return Ok(());
            }
            Ok(SessionEnd::Closed) => {
                info!("会话正常结束");
                backoff.reset();
            }
            // 错误不吞：连不上的原因必须能从日志里看出来
            Err(e) => warn!("会话结束: {e:#}"),
        }
        let delay = backoff.next_delay();
        info!(?delay, "等待后重连");
        tokio::time::sleep(delay).await;
    }
}

/// 一次会话是怎么结束的。
#[derive(Debug, PartialEq, Eq)]
enum SessionEnd {
    /// 对端正常关闭，重连即可
    Closed,
    /// 已经换好新二进制，进程应当退出让服务管理器拉起新版本
    Upgraded,
}

/// 在内置公共 CA 之外，**再**信任用户给的 CA 证书。
///
/// 用途：面板用了私有 CA 或自签证书（内网部署、或者还没配域名的时候）。
/// 默认只信任内置的 Mozilla CA 列表，那种证书会被直接拒掉 ——
/// 这是对的，但得给用户一条明路，否则「内置 TLS」这个功能等于只能配公网证书。
///
/// **只加不减**：公共 CA 照旧有效，证书链、有效期、主机名一样要验。
/// 这里没有任何「跳过验证」的开关，也不打算加 —— 那等于把 TLS 关掉还留个假象。
fn tls_connector(path: &str) -> Result<tokio_tungstenite::Connector> {
    use tokio_rustls::rustls::{ClientConfig, RootCertStore};

    let pem = std::fs::read(path).with_context(|| format!("读不到 CA 证书：{path}"))?;
    let mut roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let mut added = 0usize;
    for cert in rustls_pemfile::certs(&mut pem.as_slice()) {
        let cert = cert.with_context(|| format!("解析 CA 证书失败：{path}"))?;
        roots
            .add(cert)
            .with_context(|| format!("这不是一份有效的 CA 证书：{path}"))?;
        added += 1;
    }
    if added == 0 {
        bail!("{path} 里没有找到任何证书（PEM 里要有 BEGIN CERTIFICATE 段）");
    }
    info!(path, count = added, "已加载额外信任的 CA");

    let config = ClientConfig::builder_with_provider(std::sync::Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .context("构造 TLS 配置失败")?
    .with_root_certificates(roots)
    .with_no_client_auth();

    Ok(tokio_tungstenite::Connector::Rustls(std::sync::Arc::new(
        config,
    )))
}

/// 一次完整的连接会话。返回 `Ok` 表示正常结束，`Err` 表示异常断开。
///
/// `pending` 是上一次更新留下的试用状态：连上后（收到 `Welcome`）就提交，
/// 并置为 `None` —— 这是第 5 道防线里「试用成功」的唯一判定点。
async fn run_session(
    cfg: &Config,
    collector: &mut Platform,
    pending: Option<&update::state::Pending>,
    committed: &std::sync::atomic::AtomicBool,
) -> Result<SessionEnd> {
    let url = cfg.ws_url();
    let mut req = url
        .as_str()
        .into_client_request()
        .with_context(|| format!("非法的 server 地址: {url}"))?;
    req.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", cfg.token)
            .parse()
            .context("token 含有非法的 HTTP 头字符")?,
    );
    req.headers_mut()
        .insert("Sec-WebSocket-Protocol", WS_SUBPROTOCOL.parse()?);

    // 配了私有 CA 就自己造连接器，否则用默认的（内置公共 CA 列表）
    let connector = match &cfg.ca_cert {
        Some(path) => Some(tls_connector(path)?),
        None => None,
    };
    let (stream, resp) =
        tokio_tungstenite::connect_async_tls_with_config(req, None, false, connector)
            .await
            .with_context(|| format!("连接 {url} 失败"))?;
    info!(status = ?resp.status(), "已连接");

    let (mut tx, mut rx) = stream.split();

    // 每次重连都重新探测能力 —— 机器可能加了显卡，管理员可能改了 ping_group_range
    let facts = collector.facts();
    let icmp_available = facts.capabilities.icmp_unprivileged;
    let mut hello = hello_from(facts);
    // 采集器只知道「这台机器能不能自更新」（有没有内置公钥、目录能不能写），
    // 还要 AND 上配置项（update_base 是否配了、有没有 --disable-auto-update）。
    // 面板据此显示「可升级」还是「需手动升级」—— 必须如实。
    hello.capabilities.self_update = cfg.self_update_ready();
    log_capabilities(&hello.capabilities);
    tx.send(to_msg(&AgentMsg::Hello(hello))?).await?;

    let mut runtime = RuntimeConfig::default();
    let mut ticker = new_ticker(runtime.interval_s);
    // 探测器每秒 tick 一次；具体哪个任务到点由它自己判断
    let mut prober = Prober::new(icmp_available);
    let mut ping_ticker = tokio::time::interval(Duration::from_secs(1));
    ping_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let m = collector.sample(&runtime);
                debug!(cpu_pct = m.cpu_pct, rx = m.net.rx_speed, tx = m.net.tx_speed, "上报");
                tx.send(to_msg(&AgentMsg::Metrics(m))?).await.context("发送指标失败")?;
            }

            _ = ping_ticker.tick(), if prober.has_tasks() => {
                if let Some(batch) = prober.tick(std::time::Instant::now()).await {
                    debug!(count = batch.len(), "上报延迟结果");
                    tx.send(to_msg(&AgentMsg::PingResults { results: batch })?).await
                        .context("发送延迟结果失败")?;
                }
            }

            incoming = rx.next() => {
                match incoming {
                    None => {
                        // 断开前把攒着的结果尽量发出去，别白探一场
                        let left = prober.drain();
                        if !left.is_empty() {
                            debug!(count = left.len(), "连接关闭前补发延迟结果");
                            let _ = tx.send(to_msg(&AgentMsg::PingResults { results: left })?).await;
                        }
                        return Ok(SessionEnd::Closed);
                    }
                    Some(Err(e)) => bail!("读取失败: {e}"),
                    Some(Ok(Message::Close(frame))) => {
                        info!(?frame, "server 主动关闭");
                        return Ok(SessionEnd::Closed);
                    }
                    // tungstenite 自动回 Pong，这里不用做事
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    Some(Ok(Message::Text(txt))) => {
                        match serde_json::from_str::<ServerMsg>(txt.as_str()) {
                            Ok(ServerMsg::Welcome(w)) => {
                                info!(server_time = w.server_time, interval_s = w.interval_s, "收到 Welcome");
                                // 连上了 = 新版本可用。提交更新、删掉备份与状态文件。
                                // 先置标志再落盘：看门狗只看标志，
                                // 反过来的话它可能在两步之间醒来，把一次成功的更新回滚掉。
                                if let Some(p) = pending {
                                    if !committed.swap(true, std::sync::atomic::Ordering::SeqCst) {
                                        update::commit(p);
                                    }
                                }
                                let want = w.interval_s.clamp(1, 60);
                                if want != runtime.interval_s {
                                    runtime.interval_s = want;
                                    ticker = new_ticker(want);
                                }
                            }
                            Ok(ServerMsg::PingTasks { tasks }) => {
                                info!(count = tasks.len(), "收到延迟探测任务");
                                prober.set_tasks(tasks, std::time::Instant::now());
                            }
                            Ok(ServerMsg::Config(c)) => {
                                // 强制校验：server 被攻破也不能让 agent 做出格的事
                                let c = c.sanitize();
                                info!(interval_s = c.interval_s, gpu = c.gpu_enabled,
                                      include = ?c.net_include, exclude = ?c.net_exclude, "收到运行期配置");
                                if c.interval_s != runtime.interval_s {
                                    ticker = new_ticker(c.interval_s);
                                }
                                runtime = c;
                            }
                            Ok(ServerMsg::Upgrade { version }) => {
                                if !cfg.self_update_ready() {
                                    warn!(%version, "收到升级指令但本机自更新不可用，已忽略");
                                    continue;
                                }
                                info!(%version, "收到升级指令");
                                // 注意：整条链路的安全判定都在 update::perform 里 ——
                                // server 给的只有这个版本号，下载源来自本地配置
                                match update::perform(&cfg.update_base, AGENT_VERSION, &version).await {
                                    Ok(true) => return Ok(SessionEnd::Upgraded),
                                    // 拒绝（降级、没公钥）不是错误，继续正常跑
                                    Ok(false) => {}
                                    // 失败也继续跑：更新失败绝不能让监控本身停掉
                                    Err(e) => error!("更新失败，继续使用当前版本: {e:#}"),
                                }
                            }
                            // 未知消息只记日志不断开：老 agent 连新 server 必须能继续工作
                            Err(e) => warn!("无法解析 server 消息，已忽略: {e}"),
                        }
                    }
                    Some(Ok(other)) => debug!(?other, "忽略非文本消息"),
                }
            }
        }
    }
}

fn new_ticker(interval_s: u8) -> tokio::time::Interval {
    let mut t = tokio::time::interval(Duration::from_secs(u64::from(interval_s.clamp(1, 60))));
    // 采集卡顿后不要把错过的 tick 补做一遍，直接跳到下一个周期
    t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    t
}

fn to_msg(m: &AgentMsg) -> Result<Message> {
    Ok(Message::Text(serde_json::to_string(m)?.into()))
}
