//! 公开的实时推送（`WS /api/v1/public/ws`）。
//!
//! **帧内只带高频变化的字段**（在线状态、CPU、内存、网速、延迟）。
//! 静态信息与账单走 REST 拿一次就够 —— 这让 200 台机器的一帧维持在
//! 约 12 KB，压缩后约 2 KB。

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use tracing::debug;

use super::{Ctx, PublicMode};
use crate::state::now_unix;

/// 推送间隔。与 agent 的默认采集间隔一致 —— 更快没有新数据可推。
const TICK: Duration = Duration::from_secs(2);

pub fn routes() -> Router<Ctx> {
    Router::new().route("/api/v1/public/ws", get(upgrade))
}

/// 客户端发来的订阅。
#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum ClientMsg {
    /// 只订阅可见的机器，避免给 200 台全量推送
    Subscribe {
        #[serde(default)]
        servers: Vec<String>,
        #[serde(default)]
        summary: bool,
    },
}

async fn upgrade(ws: WebSocketUpgrade, State(ctx): State<Ctx>) -> Response {
    if ctx.config.public_mode == PublicMode::Private {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            "本站为私有模式，请先登录",
        )
            .into_response();
    }
    ws.on_upgrade(move |socket| run(socket, ctx))
}

async fn run(mut socket: WebSocket, ctx: Ctx) {
    // 订阅集为空 = 还没收到 subscribe，先什么都不推
    let mut subscribed: Option<Vec<String>> = None;
    let mut want_summary = true;
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let Some(subs) = subscribed.as_ref() else { continue };
                let frame = build_tick(&ctx, subs, want_summary);
                let Ok(txt) = serde_json::to_string(&frame) else { continue };
                if socket.send(Message::Text(txt.into())).await.is_err() {
                    break; // 客户端断开
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    None | Some(Err(_)) => break,
                    Some(Ok(Message::Close(_))) => break,
                    Some(Ok(Message::Text(t))) => {
                        match serde_json::from_str::<ClientMsg>(t.as_str()) {
                            Ok(ClientMsg::Subscribe { servers, summary }) => {
                                // 上限防护：一个客户端不该能让服务端为它
                                // 遍历任意多的 uuid
                                let mut v = servers;
                                v.truncate(1000);
                                debug!(count = v.len(), "订阅更新");
                                subscribed = Some(v);
                                want_summary = summary;
                            }
                            // 认不出的消息忽略，不断开 —— 新版前端可能发了
                            // 我们还不认识的东西
                            Err(e) => debug!("忽略无法解析的客户端消息: {e}"),
                        }
                    }
                    // Ping 由 axum 自动回 Pong
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

/// 构造一帧。
///
/// 直接从内存实时层读，**0 次数据库查询**。
/// 每个连接自己构造而不是全局广播：200 台的一帧只要几十微秒，
/// 而广播会引入「慢客户端拖累所有人」的问题。
fn build_tick(ctx: &Ctx, subs: &[String], want_summary: bool) -> serde_json::Value {
    let mut servers = serde_json::Map::new();
    // 空订阅列表 = 订阅全部（首页就是这个用法）
    let all = subs.is_empty();

    // 与公开列表同源：以数据库为准，实时层里没有的按离线推 ——
    // 否则刚添加的机器在页面上会一直保持初始状态不更新
    let values = ctx.state.values();
    let live: std::collections::HashMap<_, _> = ctx
        .state
        .list()
        .into_iter()
        .map(|s| (s.id.clone(), s))
        .collect();

    for v in values.servers.iter().filter(|v| !v.hidden) {
        let s = live
            .get(&v.uuid)
            .cloned()
            .unwrap_or_else(|| v.offline_view());
        if !all && !subs.contains(&s.id) {
            continue;
        }
        servers.insert(
            s.id.clone(),
            serde_json::json!({
                "online": s.online,
                "cpu": s.cpu_pct,
                "mem_used": s.mem.used,
                "mem_pct": s.mem.pct,
                "disk_pct": s.disk.pct,
                "net_in": s.net.rx_speed,
                "net_out": s.net.tx_speed,
                "rtt": s.latency.map(|l| l.rtt_ms),
                "loss": s.latency.map(|l| l.loss_pct),
                "uptime_s": s.uptime_s,
            }),
        );
    }

    let mut out = serde_json::json!({
        "t": "tick",
        "ts": now_unix(),
        "servers": servers,
    });
    if want_summary {
        let sm = ctx.state.summary();
        let (rx, tx) = ctx.state.total_speed();
        out["summary"] = serde_json::json!({
            "total": sm.total, "online": sm.online, "offline": sm.offline,
            "in_speed": rx, "out_speed": tx,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_message_parses_with_defaults() {
        let m: ClientMsg = serde_json::from_str(r#"{"t":"subscribe"}"#).unwrap();
        match m {
            ClientMsg::Subscribe { servers, summary } => {
                assert!(servers.is_empty(), "空列表 = 订阅全部");
                assert!(!summary);
            }
        }
        let m: ClientMsg =
            serde_json::from_str(r#"{"t":"subscribe","servers":["a","b"],"summary":true}"#)
                .unwrap();
        match m {
            ClientMsg::Subscribe { servers, summary } => {
                assert_eq!(servers, vec!["a", "b"]);
                assert!(summary);
            }
        }
    }

    #[test]
    fn unknown_client_messages_are_rejected_not_fatal() {
        // 调用方会忽略解析失败并保持连接
        assert!(serde_json::from_str::<ClientMsg>(r#"{"t":"nope"}"#).is_err());
        assert!(serde_json::from_str::<ClientMsg>("garbage").is_err());
    }
}
