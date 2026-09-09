//! M7 容量验证：开 N 路真实 WebSocket agent 连接，按采集间隔持续上报。
//!
//! 和 `pulse-loadgen`（直接写库，测存储体积）互补：这个测的是**在线稳态**——
//! server 的 CPU / 内存，以及前端在 N 台机器实时推送下的渲染开销。
//!
//! 刻意复用 `pulse-proto` 的类型而不是手写 JSON：协议一改，这里就编译不过，
//! 不会出现「压测用的报文和真 agent 早就不是一回事」这种假验证。
//!
//! 用法：
//!   pulse-wsload --url ws://127.0.0.1:25831/api/v1/agent/ws \
//!                --tokens /tmp/tokens.txt [--interval 10] [--seconds 60]
use anyhow::{Context, Result};
use futures_util::SinkExt;
use pulse_proto::{
    AgentMsg, Capabilities, DiskUsage, Hello, Mem, Metrics, NetStat, WS_SUBPROTOCOL,
};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

struct Args {
    url: String,
    tokens: String,
    interval_s: u64,
    seconds: u64,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        url: "ws://127.0.0.1:25831/api/v1/agent/ws".into(),
        tokens: String::new(),
        interval_s: 10,
        seconds: 60,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut val =
            || -> Result<String> { it.next().with_context(|| format!("{k} 缺少参数值")) };
        match k.as_str() {
            "--url" => a.url = val()?,
            "--tokens" => a.tokens = val()?,
            "--interval" => a.interval_s = val()?.parse()?,
            "--seconds" => a.seconds = val()?.parse()?,
            "-h" | "--help" => {
                println!(
                    "用法: pulse-wsload --tokens FILE [--url URL] [--interval S] [--seconds S]"
                );
                std::process::exit(0);
            }
            other => anyhow::bail!("未知参数: {other}"),
        }
    }
    if a.tokens.is_empty() {
        anyhow::bail!("--tokens 必填：每行一个 agent token");
    }
    Ok(a)
}

fn hello(idx: usize) -> Hello {
    Hello {
        proto_version: 1,
        agent_version: "wsload".into(),
        hostname: format!("bench-{idx:03}"),
        os: "Debian 12".into(),
        arch: "x86_64".into(),
        cpu_cores: 2,
        boot_at: now() - 86_400 * 30,
        kernel: Some("6.1.0".into()),
        cpu_model: Some("AMD EPYC 7003".into()),
        virtualization: Some("kvm".into()),
        mem_total: 2 << 30,
        swap_total: 1 << 30,
        disk_total: 40 << 30,
        interfaces: vec!["eth0".into()],
        capabilities: Capabilities {
            icmp_unprivileged: true,
            proc_count: true,
            load_average: true,
            tcp_conn_count: true,
            ..Default::default()
        },
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// 每一列都填上真实量级的值 —— 半空的报文会让 server 走不到真正的写路径。
fn metrics(rng: &mut impl Rng, rx: &mut u64, tx: &mut u64) -> Metrics {
    let rx_speed = rng.random_range(0..12_500_000u64);
    let tx_speed = rng.random_range(0..12_500_000u64);
    *rx += rx_speed;
    *tx += tx_speed;
    let total = 2u64 << 30;
    let free = rng.random_range(total / 20..total / 2);
    Metrics {
        ts: now(),
        cpu_pct: rng.random_range(200..9_500),
        load: [
            rng.random_range(0..400),
            rng.random_range(0..400),
            rng.random_range(0..400),
        ],
        mem: Mem {
            total,
            free,
            available: free + rng.random_range(0..total / 4),
            buffers: rng.random_range(0..total / 16),
            cached: rng.random_range(0..total / 8),
            swap_total: 1 << 30,
            swap_free: rng.random_range(0..1 << 30),
        },
        disk: DiskUsage {
            total: 40 << 30,
            used: rng.random_range(4 << 30..36 << 30),
        },
        net: NetStat {
            rx_bytes: *rx,
            tx_bytes: *tx,
            rx_speed,
            tx_speed,
            ifaces: vec!["eth0".into()],
        },
        tcp_conn: Some(rng.random_range(10..900)),
        udp_conn: Some(rng.random_range(0..60)),
        proc_count: Some(rng.random_range(80..400)),
        gpu: None,
        cpu_temp: Some(rng.random_range(300..800)),
        uptime_s: 86_400 * 30,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;
    let tokens: Vec<String> = std::fs::read_to_string(&args.tokens)
        .with_context(|| format!("读不到 {}", args.tokens))?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    if tokens.is_empty() {
        anyhow::bail!("{} 里没有任何 token", args.tokens);
    }

    println!("连接 {} 路 agent → {}", tokens.len(), args.url);
    let sent = Arc::new(AtomicU64::new(0));
    let connected = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));

    let mut tasks = Vec::new();
    for (i, token) in tokens.into_iter().enumerate() {
        let (url, interval_s) = (args.url.clone(), args.interval_s);
        let (sent, connected, failed) = (sent.clone(), connected.clone(), failed.clone());
        tasks.push(tokio::spawn(async move {
            // 连接不要同时发起：200 路一起握手会把首个采样点挤成一个尖峰，
            // 测出来的不是稳态。按采集间隔均匀铺开。
            let jitter = (i as u64 * interval_s * 1000 / 200) % (interval_s * 1000);
            tokio::time::sleep(Duration::from_millis(jitter)).await;
            if let Err(e) = session(&url, &token, i, interval_s, &sent, &connected).await {
                failed.fetch_add(1, Ordering::Relaxed);
                eprintln!("agent {i} 断开: {e:#}");
            }
        }));
    }

    // 每 10 秒打一次进度，便于在另一个终端同时采 CPU
    let deadline = tokio::time::Instant::now() + Duration::from_secs(args.seconds);
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    tick.tick().await;
    loop {
        tokio::select! {
            _ = tick.tick() => println!(
                "  在线 {} / 失败 {} / 已发 {} 条",
                connected.load(Ordering::Relaxed),
                failed.load(Ordering::Relaxed),
                sent.load(Ordering::Relaxed),
            ),
            _ = tokio::time::sleep_until(deadline) => break,
        }
    }
    println!(
        "结束：在线 {} / 失败 {} / 共发 {} 条",
        connected.load(Ordering::Relaxed),
        failed.load(Ordering::Relaxed),
        sent.load(Ordering::Relaxed)
    );
    for t in tasks {
        t.abort();
    }
    Ok(())
}

async fn session(
    url: &str,
    token: &str,
    idx: usize,
    interval_s: u64,
    sent: &AtomicU64,
    connected: &AtomicU64,
) -> Result<()> {
    let mut req = url.into_client_request().context("非法 URL")?;
    req.headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse()?);
    req.headers_mut()
        .insert("Sec-WebSocket-Protocol", WS_SUBPROTOCOL.parse()?);
    let (mut stream, _) = tokio_tungstenite::connect_async(req)
        .await
        .context("连接失败")?;
    connected.fetch_add(1, Ordering::Relaxed);

    stream
        .send(Message::Text(
            serde_json::to_string(&AgentMsg::Hello(hello(idx)))?.into(),
        ))
        .await?;

    let mut rng = StdRng::from_os_rng();
    let (mut rx, mut tx) = (0u64, 0u64);
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_s));
    loop {
        ticker.tick().await;
        let m = AgentMsg::Metrics(metrics(&mut rng, &mut rx, &mut tx));
        stream
            .send(Message::Text(serde_json::to_string(&m)?.into()))
            .await?;
        sent.fetch_add(1, Ordering::Relaxed);
    }
}
