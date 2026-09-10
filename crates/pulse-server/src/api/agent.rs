//! Agent 接入。
//!
//! **只有这一个接口。** agent 侧没有任何其他可调的服务端接口，
//! 服务端也没有任何可调的 agent 接口。

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tokio::sync::mpsc;
use tokio::time::{timeout, Instant};
use tracing::{debug, info, warn};

use super::{bearer, client_ip, Ctx};
use crate::auth::token_hash;
use crate::state::now_unix;
use crate::store::{PingLayer, PingRow, ServerFacts, ServerRecord};
use pulse_proto::{AgentMsg, ServerMsg, Welcome, WS_SUBPROTOCOL};

/// 每隔这么久主动给 agent 发一个 Ping。
///
/// 跨境长链路（德国面板 ↔ 港台 agent）上连接会被中间设备**静默掐断**：
/// 不发 RST、包直接黑洞。原先两边都没有心跳，服务端只读不写，
/// 于是这条死连接会一直挂着 —— 既不报错也不退出。
const HEARTBEAT: Duration = Duration::from_secs(20);
/// 这么久没收到 agent 的任何帧（指标、Pong 都算）就判定连接已死。
///
/// 老版本 agent 不会主动发 Ping，但 tungstenite 会自动回 Pong，
/// 所以上面的心跳对老 agent 一样有效，不会误杀。
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// 单次写的上限。链路黑洞时写会卡在 TCP 重传上，默认要十几分钟才报错。
const WRITE_TIMEOUT: Duration = Duration::from_secs(15);

pub fn routes() -> Router<Ctx> {
    Router::new().route("/api/v1/agent/ws", get(agent_ws))
}

async fn agent_ws(
    ws: WebSocketUpgrade,
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
) -> Response {
    let Some(token) = bearer(&headers) else {
        // 凭据问题一律返回 HTTP 401 而不是 WS close：agent 据此判断
        // 「凭据错了，不要无限重试」，而不是当成网络抖动
        return (StatusCode::UNAUTHORIZED, "missing bearer token").into_response();
    };

    // M3 起 token 必须在数据库里查得到。查不到就是无效凭据。
    // 比对的是 SHA-256，数据库里没有明文。
    let server = match ctx.store.get_server_by_token(&token_hash(&token)).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            warn!(peer = %peer.ip(), "agent 凭据无效");
            return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
        }
        Err(e) => {
            tracing::error!("查询 agent 凭据失败: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal").into_response();
        }
    };

    let ip = client_ip(&headers, peer.ip(), ctx.config.trusted_proxy_hops);
    ws.protocols([WS_SUBPROTOCOL])
        .on_upgrade(move |socket| handle(socket, ctx, server, ip))
}

async fn handle(socket: WebSocket, ctx: Ctx, server: ServerRecord, ip: std::net::IpAddr) {
    let uuid = server.uuid.clone();
    let id = server.id;
    info!(id, name = %server.name, %ip, "agent 已连接");

    // 同一 token 只允许一个连接：新连接会把旧的挤掉。
    // 实现是替换 state 里的发送端 —— 旧连接的 rx 随之关闭，它的循环据此退出。
    // 返回的 conn 是这条连接的代号，断开时凭它注销，见 on_disconnect。
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();
    let conn = ctx.state.on_connect(&uuid, tx.clone());
    ctx.state.set_db_id(&uuid, id);
    // GeoIP：**按连入的 IP 判国家**，不用后台手填。
    // `location_manual` 为真表示用户自己填过坐标/国家，那就以他为准 ——
    // GeoIP 到国家级就到头了，用户比它更知道机器在哪个机房。
    let mut server = server;
    if !server.location_manual {
        if let Some(cc) = ctx.geoip.load().lookup(ip).map(str::to_owned) {
            if server.country_code.as_deref() != Some(cc.as_str()) {
                info!(id, %ip, %cc, "按 IP 解析出国家");
                let patch = crate::store::ServerPatch {
                    country_code: Some(Some(cc.clone())),
                    ..Default::default()
                };
                if let Err(e) = ctx.store.update_server(id, &patch).await {
                    warn!(id, "写入 GeoIP 国家失败: {e}");
                }
                server.country_code = Some(cc);
            }
        }
    }
    ctx.state.set_meta(&uuid, (&server).into());

    // 连上就把当前配置发过去。agent 重连后立刻拿到最新配置，
    // 不需要等后台再改一次。
    let cfg = ctx.store.get_runtime_config(id).await.unwrap_or_default();
    let _ = tx.send(ServerMsg::Welcome(Welcome {
        server_time: now_unix(),
        interval_s: cfg.interval_s,
    }));
    let _ = tx.send(ServerMsg::Config(cfg));

    // 连上就把该跑的探测任务发过去。作用范围在服务端解析，
    // agent 只管执行 —— 它不需要知道分组的存在。
    match ctx.store.ping_tasks_for_server(id).await {
        Ok(tasks) if !tasks.is_empty() => {
            info!(id, count = tasks.len(), "下发延迟探测任务");
            let _ = tx.send(ServerMsg::PingTasks { tasks });
        }
        Ok(_) => {}
        Err(e) => warn!(id, "读取探测任务失败: {e}"),
    }
    // 本地这份发送端必须放掉。留着的话 rx 永远不会关闭 ——
    // 新连接替换掉 state 里的发送端之后，旧连接察觉不到自己已被取代，
    // 只能挂在一条（多半已经死了的）socket 上干等。
    drop(tx);

    let (mut sink, mut stream) = {
        use futures_util::StreamExt;
        socket.split()
    };

    let mut heartbeat = tokio::time::interval_at(Instant::now() + HEARTBEAT, HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_rx = Instant::now();

    loop {
        tokio::select! {
            // ── 下行：后台改配置时通过 channel 立刻推下去 ──
            out = rx.recv() => {
                use futures_util::SinkExt;
                let Some(msg) = out else {
                    // 发送端全没了 = 同一 token 的新连接已经顶替了这一条
                    info!(id, "已被同一 token 的新连接取代，关闭旧连接");
                    let close = Message::Close(Some(axum::extract::ws::CloseFrame {
                        code: axum::extract::ws::close_code::NORMAL,
                        reason: "replaced by a newer connection".into(),
                    }));
                    let _ = timeout(WRITE_TIMEOUT, sink.send(close)).await;
                    break;
                };
                // 错误不吞：序列化失败过一次（内部标签枚举装 Vec），
                // 当时这里是 `let Ok(..) else { continue }`，表现成
                // 「服务端说发了、agent 说没收到」，查了很久
                let txt = match serde_json::to_string(&msg) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::error!(id, "下发消息序列化失败（协议 bug）: {e}");
                        continue;
                    }
                };
                if !matches!(timeout(WRITE_TIMEOUT, sink.send(Message::Text(txt.into()))).await, Ok(Ok(()))) {
                    warn!(id, "下发失败或超时，连接已断");
                    break;
                }
            }

            // ── 心跳：顺带检查对端是否还活着 ──
            _ = heartbeat.tick() => {
                use futures_util::SinkExt;
                let idle = last_rx.elapsed();
                if idle > IDLE_TIMEOUT {
                    warn!(id, idle_s = idle.as_secs(), "心跳超时：长时间没收到 agent 的任何数据，判定连接已死");
                    break;
                }
                if !matches!(timeout(WRITE_TIMEOUT, sink.send(Message::Ping(Default::default()))).await, Ok(Ok(()))) {
                    warn!(id, "发送心跳失败或超时，连接已断");
                    break;
                }
            }

            // ── 上行：agent 的 Hello / Metrics ──
            inc = { use futures_util::StreamExt; stream.next() } => {
                let Some(msg) = inc else { break };
                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => { warn!(id, "读取失败: {e}"); break; }
                };
                // 任何帧都算活着，包括 Pong
                last_rx = Instant::now();
                match msg {
                    Message::Text(txt) => match serde_json::from_str::<AgentMsg>(txt.as_str()) {
                        Ok(AgentMsg::Hello(h)) => {
                            info!(id, hostname = %h.hostname, os = %h.os,
                                  agent = %h.agent_version, "收到 Hello");
                            let facts = ServerFacts {
                                name: Some(h.hostname.clone()),
                                os: Some(h.os.clone()),
                                kernel: h.kernel.clone(),
                                arch: Some(h.arch.clone()),
                                virtualization: h.virtualization.clone(),
                                cpu_model: h.cpu_model.clone(),
                                cpu_cores: Some(i64::from(h.cpu_cores)),
                                mem_total: Some(h.mem_total as i64),
                                swap_total: Some(h.swap_total as i64),
                                disk_total: Some(h.disk_total as i64),
                                agent_version: Some(h.agent_version.clone()),
                                boot_at: Some(h.boot_at),
                                capabilities: serde_json::to_string(&h.capabilities).ok(),
                                last_ip: Some(ip.to_string()),
                            };
                            // 写库失败不断开连接：指标先进内存，比把 agent 踢掉好
                            if let Err(e) = ctx.store.update_server_facts(id, &facts, now_unix()).await {
                                warn!(id, "写入静态信息失败，本次仅内存: {e}");
                            }
                            ctx.state.on_hello(&uuid, h);
                        }
                        Ok(AgentMsg::Metrics(m)) => {
                            debug!(id, cpu_pct = m.cpu_pct, "收到指标");
                            ctx.state.on_metrics(&uuid, m);
                        }
                        Ok(AgentMsg::PingResults { results: rs }) => {
                            debug!(id, count = rs.len(), "收到延迟结果");
                            let rows: Vec<PingRow> = rs.iter().map(|r| PingRow {
                                server_id: id,
                                task_id: i64::from(r.task_id),
                                ts: r.ts,
                                // 微秒。全丢包时是 None，不能写 0 ——
                                // 0 微秒的延迟会在图上画出一条假的贴地线
                                rtt_avg: r.rtt_avg_us.map(i64::from),
                                rtt_min: r.rtt_min_us.map(i64::from),
                                rtt_max: r.rtt_max_us.map(i64::from),
                                sent: i64::from(r.sent),
                                recv: i64::from(r.recv),
                            }).collect();
                            // 延迟结果直接落盘（不像指标那样先进内存）：
                            // 它本来就是分钟粒度，量也小得多
                            if let Err(e) = ctx.store.insert_ping(PingLayer::Raw, &rows).await {
                                warn!(id, "写入延迟结果失败: {e}");
                            }
                            // 顺手在内存里留一份：服务器卡片要显示延迟/丢包（R16），
                            // 而首页不允许查库。**整批都交给 state**，由它按窗口汇总 ——
                            // 只取最新一条的话，多任务时卡片永远只看得到其中一个，
                            // 单分钟 3 个包的丢包率也几乎总是 0。
                            let samples: Vec<_> = rs.iter().map(|r| crate::state::PingSample {
                                task_id: i64::from(r.task_id),
                                ts: r.ts,
                                sent: u32::from(r.sent),
                                recv: u32::from(r.recv),
                                rtt_avg_us: r.rtt_avg_us,
                            }).collect();
                            ctx.state.on_ping(&uuid, &samples);
                            if rs.iter().any(|r| r.fallback) {
                                debug!(id, "该机器的 ICMP 任务已回落 TCP");
                            }
                        }
                        // 单条坏消息不该杀掉整条连接 —— 新版 agent 可能发了我们还不认识的消息
                        Err(e) => warn!(id, "无法解析 agent 消息，已忽略: {e}"),
                    },
                    Message::Close(_) => break,
                    _ => {} // Ping 由 axum 自动回 Pong；Pong 只用来刷新 last_rx
                }
            }
        }
    }

    info!(id, "agent 已断开");
    // 带上连接代号：如果这条已经被新连接顶替，这里什么都不会做 ——
    // 否则旧连接迟到的断开会把正在正常上报的新连接标成离线
    ctx.state.on_disconnect(&uuid, conn);
}
