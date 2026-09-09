//! 后台定时任务。
//!
//! 任务清单与周期共同约束：
//! **有超时、有重试上限、失败只记日志不 panic** —— 单个任务挂掉不能拖垮整个进程。

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, error, info, warn};

pub mod geoip;
pub mod notify;
pub mod rate;
pub mod traffic;
pub mod value;

use chrono_tz::Tz;

use crate::auth::LoginLimiter;
use crate::state::AppState;
use crate::store::{MetricLayer, RetentionPolicy, Storage};

/// 单个任务允许跑多久。超时说明出了问题（锁死、磁盘卡住），
/// 宁可放弃这一轮等下一轮，也不能让它无限期占住写连接。
const TASK_TIMEOUT: Duration = Duration::from_secs(120);

/// 起一个周期任务，自带超时、错误吞掉（只记日志）、以及首轮延迟。
fn spawn_periodic<F, Fut>(name: &'static str, period: Duration, first_delay: Duration, mut f: F)
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send,
{
    tokio::spawn(async move {
        tokio::time::sleep(first_delay).await;
        let mut tick = interval(period);
        // 任务跑超时后不要把错过的 tick 补做一遍，直接跳到下一个周期
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let t0 = Instant::now();
            match tokio::time::timeout(TASK_TIMEOUT, f()).await {
                Ok(Ok(())) => debug!(task = name, elapsed_ms = t0.elapsed().as_millis(), "完成"),
                Ok(Err(e)) => error!(task = name, "失败: {e:#}"),
                Err(_) => warn!(
                    task = name,
                    timeout_s = TASK_TIMEOUT.as_secs(),
                    "超时，跳过本轮"
                ),
            }
        }
    });
}

/// 启动全部后台任务。
pub fn spawn_all(state: AppState, store: Arc<dyn Storage>, retention: RetentionPolicy) {
    // ── flush：内存实时层 → 分钟层，每 60 秒一次事务 ──
    {
        let (state, store) = (state.clone(), store.clone());
        spawn_periodic(
            "flush_minute",
            Duration::from_secs(60),
            Duration::from_secs(60),
            move || {
                let (state, store) = (state.clone(), store.clone());
                async move {
                    // 只 flush 已经结束的那一分钟；进行中的留到下一轮
                    let minute = (crate::state::now_unix() / 60 - 1) * 60;
                    let rows = state.snapshot_minute(minute);
                    if rows.is_empty() {
                        return Ok(());
                    }
                    let n = rows.len();
                    let t0 = Instant::now();
                    store.insert_metrics(MetricLayer::Minute, &rows).await?;
                    let ms = t0.elapsed().as_millis();
                    info!(rows = n, elapsed_ms = ms, minute, "flush_minute");
                    // 预算来自 ；超了说明模型失效，要回去改保留策略
                    if ms > 20 {
                        warn!(elapsed_ms = ms, "flush_minute 超出 20ms 预算");
                    }
                    Ok(())
                }
            },
        );
    }

    // ── rollup：分钟 → 小时，raw → 5m → hour ──
    {
        let store = store.clone();
        spawn_periodic(
            "rollup",
            Duration::from_secs(300),
            Duration::from_secs(90),
            move || {
                let store = store.clone();
                async move {
                    // 单轮窗口有上限，所以落后时要连续跑几轮才能追平。
                    // 轮次上限是失控防护：即使追不平也必须把写连接还回去。
                    const MAX_ROUNDS: u32 = 200;
                    let mut rounds = 0;
                    loop {
                        let r = store.rollup(crate::state::now_unix()).await?;
                        rounds += 1;
                        if r.metrics_hour + r.ping_5m + r.ping_hour > 0 {
                            info!(?r, rounds, "rollup");
                        }
                        if r.caught_up {
                            break;
                        }
                        if rounds >= MAX_ROUNDS {
                            warn!(rounds, "rollup 仍有积压，本轮结束，下个周期继续");
                            break;
                        }
                        // 每轮之间让出，避免长时间连续独占写连接把上报堵住
                        tokio::task::yield_now().await;
                    }
                    Ok(())
                }
            },
        );
    }

    // ── prune：分块删除过期行 ──
    {
        let store = store.clone();
        spawn_periodic(
            "prune",
            Duration::from_secs(3600),
            Duration::from_secs(120),
            move || {
                let store = store.clone();
                async move {
                    let r = store.prune(&retention, crate::state::now_unix()).await?;
                    let deleted =
                        r.metrics_minute + r.metrics_hour + r.ping_raw + r.ping_5m + r.ping_hour;
                    if deleted > 0 {
                        info!(?r, "prune");
                    }
                    if r.max_chunk_ms > 200 {
                        warn!(max_chunk_ms = r.max_chunk_ms, "prune 单块超出 200ms 预算");
                    }
                    Ok(())
                }
            },
        );
    }
}

/// 认证相关的清理任务。
///
/// 两张表都会无界增长，不清理就是慢性内存/磁盘泄漏。
pub fn spawn_auth_maintenance(store: Arc<dyn Storage>, limiter: Arc<LoginLimiter>) {
    spawn_periodic(
        "auth_sweep",
        Duration::from_secs(3600),
        Duration::from_secs(300),
        move || {
            let (store, limiter) = (store.clone(), limiter.clone());
            async move {
                // 过了原始有效期的吊销记录再也不会被查到，可以删
                let n = store.sweep_revoked(crate::state::now_unix()).await?;
                if n > 0 {
                    debug!(rows = n, "清理过期的吊销记录");
                }
                // 限流表按 IP 增长，长期不活动的条目要回收
                limiter.sweep(Instant::now());
                Ok(())
            }
        },
    );
}

/// M5 的两个业务任务：流量结算与汇率拉取。
pub fn spawn_business(
    state: AppState,
    store: Arc<dyn Storage>,
    panel_tz: Tz,
    rate_base_url: String,
    display_currency: String,
) {
    // ── 价值缓存：每 30 秒刷一次 ──
    // 首页 summary 因此可以保持「0 次数据库查询」（M1 的约束）
    {
        let (state, store) = (state.clone(), store.clone());
        spawn_periodic(
            "value_cache",
            Duration::from_secs(30),
            Duration::from_secs(3),
            move || {
                let (state, store, cur) = (state.clone(), store.clone(), display_currency.clone());
                async move { value::refresh_once(&state, &store, &cur, crate::state::now_unix()).await }
            },
        );
    }

    // ── 流量结算：每分钟 ──
    {
        let (state, store) = (state.clone(), store.clone());
        spawn_periodic(
            "traffic_settle",
            Duration::from_secs(60),
            Duration::from_secs(30),
            move || {
                let (state, store) = (state.clone(), store.clone());
                async move {
                    traffic::settle_once(&state, &store, panel_tz, crate::state::now_unix()).await
                }
            },
        );
    }

    // ── 汇率：每 6 小时。首轮延迟 5 秒，让面板一起来就有汇率可用 ──
    {
        let store = store.clone();
        spawn_periodic(
            "exchange_rate",
            Duration::from_secs(6 * 3600),
            Duration::from_secs(5),
            move || {
                let (store, url) = (store.clone(), rate_base_url.clone());
                async move { rate::fetch_once(&store, &url, crate::state::now_unix()).await }
            },
        );
    }
}

/// 通知规则求值（M6）。每 30 秒一轮。
///
/// 条件全部在内存里算；数据库只用来读写状态机记录 ——
/// 这样即使规则很多，求值也不会变成数据库压力。
pub fn spawn_notify(
    state: AppState,
    store: Arc<dyn Storage>,
    secret: Vec<u8>,
    panel_url: String,
    tz: Tz,
) {
    spawn_periodic(
        "notify_eval",
        Duration::from_secs(30),
        // 首轮延迟 45 秒：等价值缓存先刷一遍，否则第一轮里
        // 流量与到期这两类条件全是「无法判定」
        Duration::from_secs(45),
        move || {
            let (state, store, secret, url) = (
                state.clone(),
                store.clone(),
                secret.clone(),
                panel_url.clone(),
            );
            async move {
                notify::evaluate_once(&state, &store, &secret, &url, tz, crate::state::now_unix())
                    .await
            }
        },
    );
}
