//! 延迟探测。
//!
//! 三种方式：
//!
//! | 方式 | 需要的权限 | 说明 |
//! |---|---|---|
//! | **TCP**（默认） | **无** | `connect` 计时。零特权，所以是默认 |
//! | ICMP | 无（走非特权 datagram socket） | 内核允许时才可用，否则回落 TCP:443 |
//! | HTTP | 无 | 连接 + 状态行，验证 Web 服务真的在响应 |
//!
//! **为什么默认不是 ICMP**：ICMP raw socket 需要 `CAP_NET_RAW`，
//! 与 R18「零 capability」直接冲突。

mod icmp;
mod net;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures_util::stream::{FuturesUnordered, StreamExt};
use pulse_proto::{PingKind, PingResult, PingTaskSpec};
use tracing::{debug, warn};

/// 同时在途的探测数上限。
///
/// 任务多时不加限制会突然发起几十个并发连接，容易被 VPS 商家的风控当成扫描。
const MAX_CONCURRENT: usize = 8;

/// 结果批量上报的间隔。
///
/// 每次探测都发一条消息的话，6 个目标 × 200 台就是每分钟 1200 条小消息。
const FLUSH_INTERVAL: Duration = Duration::from_secs(30);

/// 待上报缓冲的上限。网络长时间不通时不能让它无限涨。
const MAX_PENDING: usize = 4096;

/// ICMP 不可用时的回落目标端口。443 比 80 更可能开着，也更不容易被中间设备劫持。
const ICMP_FALLBACK_PORT: u16 = 443;

pub struct Prober {
    tasks: Vec<PingTaskSpec>,
    /// 每个任务下次该跑的时间
    next_due: HashMap<u32, Instant>,
    pending: Vec<PingResult>,
    last_flush: Instant,
    /// 本机是否支持非特权 ICMP。由 `Capabilities` 探测得出。
    icmp_available: bool,
}

impl Prober {
    pub fn new(icmp_available: bool) -> Self {
        Self {
            tasks: Vec::new(),
            next_due: HashMap::new(),
            pending: Vec::new(),
            last_flush: Instant::now(),
            icmp_available,
        }
    }

    /// 全量替换任务列表。
    ///
    /// 服务端下发的是全量而不是增量 —— 增量省的那点带宽不值得，
    /// 而全量替换不会因为漏掉一条删除消息就永远多探一个目标。
    pub fn set_tasks(&mut self, tasks: Vec<PingTaskSpec>, now: Instant) {
        let tasks: Vec<_> = tasks.into_iter().map(PingTaskSpec::sanitize).collect();
        // 已有任务保留原排期：每次下发配置都重新对齐的话，
        // 所有 agent 会在同一时刻探测同一个目标
        let mut next = HashMap::with_capacity(tasks.len());
        for t in &tasks {
            let due = self.next_due.get(&t.id).copied().unwrap_or_else(|| {
                // 新任务打散首次执行时间，避免 200 台机器同时开跑
                now + Duration::from_millis(u64::from(t.id % 20) * 500)
            });
            next.insert(t.id, due);
        }
        debug!(count = tasks.len(), "更新延迟探测任务");
        self.tasks = tasks;
        self.next_due = next;
    }

    pub fn has_tasks(&self) -> bool {
        !self.tasks.is_empty()
    }

    /// 跑一轮：执行到点的任务，必要时返回一批待上报的结果。
    /// 调用方每秒调一次即可。
    pub async fn tick(&mut self, now: Instant) -> Option<Vec<PingResult>> {
        let due: Vec<PingTaskSpec> = self
            .tasks
            .iter()
            .filter(|t| self.next_due.get(&t.id).is_none_or(|d| now >= *d))
            .cloned()
            .collect();

        if !due.is_empty() {
            for t in &due {
                self.next_due
                    .insert(t.id, now + Duration::from_secs(u64::from(t.interval_s)));
            }
            let icmp = self.icmp_available;
            // 并发跑，但每批不超过上限
            for chunk in due.chunks(MAX_CONCURRENT) {
                let mut futs: FuturesUnordered<_> =
                    chunk.iter().map(|t| probe(t.clone(), icmp)).collect();
                while let Some(r) = futs.next().await {
                    self.merge(r);
                }
            }
        }

        if self.pending.len() > MAX_PENDING {
            let drop_n = self.pending.len() - MAX_PENDING;
            warn!(dropped = drop_n, "延迟结果积压过多，丢弃最旧的");
            self.pending.drain(..drop_n);
        }

        if !self.pending.is_empty() && now.duration_since(self.last_flush) >= FLUSH_INTERVAL {
            self.last_flush = now;
            return Some(std::mem::take(&mut self.pending));
        }
        None
    }

    /// 把一次探测的结果并入缓冲。
    ///
    /// **按 (任务, 分钟) 合并而不是各存一条。** 存储层的最细粒度就是 1 分钟
    /// ，探测间隔小于 60 秒时会有多次结果落在同一分钟；
    /// 不合并的话服务端 upsert 只会留下最后一次，前面几次全白探了 ——
    /// 10 秒间隔下等于 5/6 的探测被静默丢弃，而丢包率也只反映最后那一次。
    fn merge(&mut self, r: PingResult) {
        let Some(slot) = self
            .pending
            .iter_mut()
            .find(|p| p.task_id == r.task_id && p.ts == r.ts)
        else {
            self.pending.push(r);
            return;
        };

        // rtt 按 recv 加权：不同样本数的平均值直接再平均是错的
        let (a, b) = (slot.recv, r.recv);
        slot.rtt_avg_us = match (slot.rtt_avg_us, r.rtt_avg_us) {
            (Some(x), Some(y)) => Some(
                ((u64::from(x) * u64::from(a) + u64::from(y) * u64::from(b))
                    / u64::from(a + b).max(1)) as u32,
            ),
            (x, y) => x.or(y),
        };
        slot.rtt_min_us = min_opt(slot.rtt_min_us, r.rtt_min_us);
        slot.rtt_max_us = slot.rtt_max_us.max(r.rtt_max_us);
        slot.sent = slot.sent.saturating_add(r.sent);
        slot.recv = slot.recv.saturating_add(r.recv);
        slot.fallback |= r.fallback;
    }

    /// 连接断开前把没发出去的结果取走。
    pub fn drain(&mut self) -> Vec<PingResult> {
        std::mem::take(&mut self.pending)
    }
}

fn min_opt(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    }
}

/// 执行一个任务的一轮探测（发 `packets` 个包）。
async fn probe(task: PingTaskSpec, icmp_available: bool) -> PingResult {
    let timeout = Duration::from_millis(u64::from(task.timeout_ms));
    // ICMP 不可用时回落 TCP:443，并在结果上打标记 —— 后台据此提示用户，
    // 而不是让他对着一个语义不同的数字发呆
    let (kind, fallback) = match (&task.kind, icmp_available) {
        (PingKind::Icmp, false) => (
            PingKind::Tcp {
                port: ICMP_FALLBACK_PORT,
            },
            true,
        ),
        (k, _) => (k.clone(), false),
    };

    let mut rtts: Vec<u32> = Vec::with_capacity(usize::from(task.packets));
    for _ in 0..task.packets {
        let r = match &kind {
            PingKind::Tcp { port } => net::tcp_probe(&task.host, *port, timeout).await,
            PingKind::Http { expect_status } => {
                net::http_probe(&task.host, *expect_status, timeout).await
            }
            PingKind::Icmp => icmp::probe(&task.host, timeout).await,
        };
        if let Some(us) = r {
            rtts.push(us);
        }
    }

    // 丢包率**不存百分比，存 sent/recv 计数** —— 上卷时求和即可，
    // 而不同样本数的百分比直接平均是错的
    PingResult {
        task_id: task.id,
        ts: crate::now_unix() / 60 * 60, // 分钟对齐
        sent: u16::from(task.packets),
        recv: rtts.len() as u16,
        rtt_min_us: rtts.iter().min().copied(),
        rtt_max_us: rtts.iter().max().copied(),
        rtt_avg_us: (!rtts.is_empty())
            .then(|| (rtts.iter().map(|x| u64::from(*x)).sum::<u64>() / rtts.len() as u64) as u32),
        fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    fn task(id: u32, kind: PingKind, host: &str) -> PingTaskSpec {
        PingTaskSpec {
            id,
            name: format!("t{id}"),
            kind,
            host: host.into(),
            interval_s: 60,
            packets: 3,
            timeout_ms: 1000,
        }
    }

    #[tokio::test]
    async fn tcp_probe_measures_a_real_listener() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move { while l.accept().await.is_ok() {} });

        let r = probe(task(1, PingKind::Tcp { port }, "127.0.0.1"), false).await;
        assert_eq!((r.sent, r.recv), (3, 3), "本地监听应当三发三收");
        assert!(r.rtt_avg_us.is_some());
        assert!(r.rtt_min_us <= r.rtt_avg_us && r.rtt_avg_us <= r.rtt_max_us);
        assert!(!r.fallback);
        assert_eq!(r.ts % 60, 0, "时间戳必须分钟对齐");
    }

    #[tokio::test]
    async fn closed_port_counts_as_loss_not_error() {
        // 端口不通就是丢包，不是异常 —— 探针不能因此崩或卡住。
        //
        // 刻意用**回环地址**而不是 RFC 5737 的不可路由地址来测「连不上」：
        // 装了系统级透明代理（Clash/Surge 之类）的机器上，对任意公网 IP 的
        // TCP connect 都会被本地代理接管而成功，这个测试会假绿。
        // 回环不走代理，所以是确定性的。
        // 这本身也是 TCP 探测的真实局限，已记进 。
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);

        let r = probe(task(1, PingKind::Tcp { port }, "127.0.0.1"), false).await;
        assert_eq!(r.sent, 3);
        assert_eq!(r.recv, 0, "连不上应当全部计为丢包");
        assert_eq!(r.rtt_avg_us, None, "全丢包时 rtt 必须是 None 而不是 0");
    }

    #[tokio::test]
    async fn syntactically_invalid_host_is_loss_not_panic() {
        let mut t = task(1, PingKind::Tcp { port: 443 }, "not a valid host\u{0}");
        t.timeout_ms = 300;
        t.packets = 1;
        assert_eq!(probe(t, false).await.recv, 0);
    }

    #[tokio::test]
    async fn icmp_falls_back_to_tcp_when_unavailable() {
        // 验收标准之一：ICMP 不可用的机器上必须自动回落并打标记
        let r = probe(task(1, PingKind::Icmp, "127.0.0.1"), false).await;
        assert!(r.fallback, "ICMP 不可用时必须标记为已回落");
    }

    #[tokio::test]
    async fn icmp_is_not_marked_fallback_when_available() {
        let r = probe(task(1, PingKind::Icmp, "127.0.0.1"), true).await;
        assert!(!r.fallback);
    }

    #[tokio::test]
    async fn scheduler_respects_interval_and_batches() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move { while l.accept().await.is_ok() {} });

        let mut p = Prober::new(false);
        let t0 = Instant::now();
        let mut spec = task(1, PingKind::Tcp { port }, "127.0.0.1");
        spec.interval_s = 10;
        spec.packets = 1;
        p.set_tasks(vec![spec], t0);

        // 首次执行有打散延迟（id=1 → 500ms），推进之后才到点
        assert!(p.tick(t0).await.is_none());
        p.tick(t0 + Duration::from_secs(1)).await;
        assert!(
            p.tick(t0 + Duration::from_secs(2)).await.is_none(),
            "还没到 flush 时间"
        );

        let batch = p.tick(t0 + FLUSH_INTERVAL + Duration::from_secs(2)).await;
        assert!(
            batch.is_some_and(|b| !b.is_empty()),
            "超过 flush 间隔后应当返回一批结果"
        );
    }

    #[tokio::test]
    async fn set_tasks_preserves_existing_schedule() {
        // 每次下发配置都重新对齐的话，所有 agent 会同时探测同一个目标
        let mut p = Prober::new(false);
        let t0 = Instant::now();
        p.set_tasks(vec![task(1, PingKind::Tcp { port: 1 }, "h")], t0);
        let due = p.next_due[&1];

        p.set_tasks(
            vec![
                task(1, PingKind::Tcp { port: 1 }, "h"),
                task(2, PingKind::Tcp { port: 2 }, "h"),
            ],
            t0 + Duration::from_secs(5),
        );
        assert_eq!(p.next_due[&1], due, "已有任务的排期不能被重置");
        assert!(p.next_due.contains_key(&2));
    }

    #[tokio::test]
    async fn removed_tasks_stop_being_probed() {
        let mut p = Prober::new(false);
        let t0 = Instant::now();
        p.set_tasks(vec![task(1, PingKind::Tcp { port: 1 }, "h")], t0);
        p.set_tasks(vec![], t0);
        assert!(!p.has_tasks());
        assert!(p.next_due.is_empty(), "删掉的任务不能继续占着排期");
    }

    #[tokio::test]
    async fn results_in_the_same_minute_are_merged_not_overwritten() {
        // 回归测试：存储层最细就是 1 分钟，探测间隔 <60 秒时同一分钟会有多次结果。
        // 不合并的话服务端 upsert 只留下最后一次 —— 10 秒间隔下 5/6 的探测白做，
        // 丢包率也只反映最后那一次。实测中就是靠「7 分钟只落了 28 行」发现的。
        let mut p = Prober::new(false);
        p.merge(PingResult {
            task_id: 1,
            ts: 60,
            sent: 3,
            recv: 3,
            rtt_min_us: Some(100),
            rtt_avg_us: Some(200),
            rtt_max_us: Some(300),
            fallback: false,
        });
        p.merge(PingResult {
            task_id: 1,
            ts: 60,
            sent: 3,
            recv: 1,
            rtt_min_us: Some(50),
            rtt_avg_us: Some(400),
            rtt_max_us: Some(400),
            fallback: false,
        });
        // 不同分钟不合并
        p.merge(PingResult {
            task_id: 1,
            ts: 120,
            sent: 3,
            recv: 3,
            ..Default::default()
        });
        // 不同任务不合并
        p.merge(PingResult {
            task_id: 2,
            ts: 60,
            sent: 3,
            recv: 3,
            ..Default::default()
        });

        assert_eq!(p.pending.len(), 3);
        let m = p
            .pending
            .iter()
            .find(|x| x.task_id == 1 && x.ts == 60)
            .unwrap();
        assert_eq!((m.sent, m.recv), (6, 4), "sent/recv 应当相加");
        assert_eq!(m.rtt_min_us, Some(50));
        assert_eq!(m.rtt_max_us, Some(400));
        // 加权平均：(200×3 + 400×1) / 4 = 250；算术平均会得到错误的 300
        assert_eq!(m.rtt_avg_us, Some(250), "rtt 必须按 recv 加权");
    }

    #[tokio::test]
    async fn merging_a_total_loss_round_keeps_rtt_from_the_good_one() {
        let mut p = Prober::new(false);
        p.merge(PingResult {
            task_id: 1,
            ts: 60,
            sent: 3,
            recv: 3,
            rtt_min_us: Some(100),
            rtt_avg_us: Some(200),
            rtt_max_us: Some(300),
            fallback: false,
        });
        // 全丢包的一轮：rtt 全 None，不能把已有的 rtt 冲掉
        p.merge(PingResult {
            task_id: 1,
            ts: 60,
            sent: 3,
            recv: 0,
            ..Default::default()
        });

        let m = &p.pending[0];
        assert_eq!((m.sent, m.recv), (6, 3));
        assert_eq!(m.rtt_avg_us, Some(200), "全丢包的一轮不能冲掉已有的 rtt");
        assert_eq!(m.rtt_min_us, Some(100));
    }

    #[tokio::test]
    async fn fallback_flag_is_sticky_across_merges() {
        let mut p = Prober::new(false);
        p.merge(PingResult {
            task_id: 1,
            ts: 60,
            sent: 1,
            recv: 1,
            ..Default::default()
        });
        p.merge(PingResult {
            task_id: 1,
            ts: 60,
            sent: 1,
            recv: 1,
            fallback: true,
            ..Default::default()
        });
        assert!(p.pending[0].fallback, "只要有一轮回落过，整条结果就该标记");
    }

    #[tokio::test]
    async fn pending_buffer_is_bounded() {
        // 网络长时间不通时缓冲不能无限涨
        let mut p = Prober::new(false);
        p.pending = (0..5000)
            .map(|i| PingResult {
                task_id: 1,
                ts: i,
                ..Default::default()
            })
            .collect();
        p.tick(Instant::now()).await;
        assert!(p.pending.len() <= MAX_PENDING, "实际 {}", p.pending.len());
        assert_eq!(p.pending.last().unwrap().ts, 4999, "丢的应当是最旧的");
    }

    #[tokio::test]
    async fn hostile_task_spec_is_clamped_before_use() {
        // 下发的任务会变成真实的出网连接
        let mut p = Prober::new(false);
        let mut t = task(1, PingKind::Tcp { port: 1 }, "h");
        t.interval_s = 0;
        t.packets = 255;
        p.set_tasks(vec![t], Instant::now());
        assert_eq!(p.tasks[0].interval_s, PingTaskSpec::MIN_INTERVAL_S);
        assert_eq!(p.tasks[0].packets, PingTaskSpec::MAX_PACKETS);
    }
}
