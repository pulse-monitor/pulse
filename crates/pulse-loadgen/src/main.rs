//! M1 容量验证工具。
//!
//! 用途只有一个：把 里的**纸面测算**变成**实测数字**，
//! 从而回答承重假设 A1 ——「200 台规模下分层 SQLite 的体积与查询延迟在预算内吗」。
//!
//! 两条纪律：
//!
//! 1. **必须走真实的存储层代码路径**（`pulse_server::store`）。自己另写一套
//!    INSERT 的话，测出来的是压测程序自己，不是实际实现。
//! 2. **必须生成满宽度的行**。SQLite 里 NULL 只占 1 字节 —— 用大部分列为空的
//!    行去测体积，会得出一个偏小 5 倍的假数字。
//!
//! 用法：
//! ```text
//! pulse-loadgen --servers 200 --days 7 --ping-tasks 6 --db sqlite:///tmp/bench.db
//! ```

use std::time::Instant;

use anyhow::{Context, Result};
use pulse_server::store::{
    sqlite::SqliteStore, MetricLayer, MetricRow, PingLayer, PingRow, RetentionPolicy, Storage,
};
use rand::{Rng, SeedableRng};

const MIB: f64 = 1024.0 * 1024.0;

struct Args {
    servers: i64,
    days: i64,
    ping_tasks: i64,
    ping_interval_s: i64,
    db: String,
    keep: bool,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut a = Args {
            servers: 200,
            days: 7,
            ping_tasks: 6,
            ping_interval_s: 60,
            db: "sqlite:///tmp/pulse-bench.db".into(),
            keep: false,
        };
        let mut it = std::env::args().skip(1);
        while let Some(k) = it.next() {
            let mut val =
                || -> Result<String> { it.next().with_context(|| format!("{k} 缺少参数值")) };
            match k.as_str() {
                "--servers" => a.servers = val()?.parse()?,
                "--days" => a.days = val()?.parse()?,
                "--ping-tasks" => a.ping_tasks = val()?.parse()?,
                "--ping-interval" => a.ping_interval_s = val()?.parse()?,
                "--db" => a.db = val()?,
                "--keep" => a.keep = true,
                "-h" | "--help" => {
                    println!("用法: pulse-loadgen [--servers N] [--days N] [--ping-tasks N] [--ping-interval S] [--db URL] [--keep]");
                    std::process::exit(0);
                }
                other => anyhow::bail!("未知参数: {other}"),
            }
        }
        Ok(a)
    }

    fn db_path(&self) -> Option<&str> {
        self.db.strip_prefix("sqlite://")
    }
}

/// 生成一行满宽度的、数值分布贴近真实的指标。
///
/// 值本身不重要，**每一列都非 NULL** 才重要 —— 这决定了体积测量是否有效。
fn gen_metric(rng: &mut impl Rng, server_id: i64, ts: i64) -> MetricRow {
    let cpu = rng.random_range(200..9_500);
    let mem_total: i64 = 1 << 30;
    let mem_free = rng.random_range(mem_total / 20..mem_total / 2);
    MetricRow {
        server_id,
        ts,
        cpu_pct: Some(cpu),
        cpu_pct_max: Some((cpu + rng.random_range(0..500)).min(10_000)),
        load1: Some(rng.random_range(0..400)),
        mem_total: Some(mem_total),
        mem_free: Some(mem_free),
        mem_available: Some(mem_free + rng.random_range(0..mem_total / 4)),
        swap_used: Some(rng.random_range(0..(1i64 << 28))),
        disk_used: Some(rng.random_range(1i64 << 32..1i64 << 34)),
        disk_total: Some(1i64 << 34),
        net_in_speed: Some(rng.random_range(0..12_500_000)),
        net_out_speed: Some(rng.random_range(0..12_500_000)),
        net_in_peak: Some(rng.random_range(0..25_000_000)),
        net_out_peak: Some(rng.random_range(0..25_000_000)),
        // 累计计数会长到很大，正是行宽的大头，不能用小数字糊弄过去
        net_in_total: Some(ts * 1_200_000 + rng.random_range(0..1_000_000)),
        net_out_total: Some(ts * 900_000 + rng.random_range(0..1_000_000)),
        tcp_conn: Some(rng.random_range(10..4_000)),
        udp_conn: Some(rng.random_range(0..200)),
        proc_count: Some(rng.random_range(60..600)),
        gpu_util: Some(rng.random_range(0..10_000)),
        gpu_mem_used: Some(rng.random_range(0..(1i64 << 33))),
        gpu_temp: Some(rng.random_range(300..850)),
        cpu_temp: Some(rng.random_range(300..900)),
    }
}

fn gen_ping(rng: &mut impl Rng, server_id: i64, task_id: i64, ts: i64, packets: i64) -> PingRow {
    let base = rng.random_range(5_000..350_000);
    let lost = if rng.random_range(0..100) < 3 {
        rng.random_range(1..=packets)
    } else {
        0
    };
    let recv = packets - lost;
    PingRow {
        server_id,
        task_id,
        ts,
        rtt_avg: (recv > 0).then_some(base),
        rtt_min: (recv > 0).then(|| base - rng.random_range(0..2_000)),
        rtt_max: (recv > 0).then(|| base + rng.random_range(0..8_000)),
        sent: packets,
        recv,
    }
}

/// 分批写入，每批打印进度。批大小取 20000 行 —— 再大只是徒增内存峰值。
const BATCH: usize = 20_000;

async fn fill_metrics(
    store: &SqliteStore,
    layer: MetricLayer,
    ids: &[i64],
    start: i64,
    step: i64,
    points: i64,
    rng: &mut impl Rng,
) -> Result<(u64, f64)> {
    let total = ids.len() as i64 * points;
    let t0 = Instant::now();
    let mut buf = Vec::with_capacity(BATCH);
    let mut done = 0i64;

    for p in 0..points {
        let ts = start + p * step;
        for &id in ids {
            buf.push(gen_metric(rng, id, ts));
            if buf.len() >= BATCH {
                store.insert_metrics(layer, &buf).await?;
                done += buf.len() as i64;
                buf.clear();
                if done % 400_000 == 0 {
                    eprint!("\r  {}: {done}/{total} 行", layer.table());
                }
            }
        }
    }
    if !buf.is_empty() {
        store.insert_metrics(layer, &buf).await?;
        done += buf.len() as i64;
    }
    let secs = t0.elapsed().as_secs_f64();
    eprintln!(
        "\r  {}: {done} 行，{secs:.1}s（{:.0} 行/秒）",
        layer.table(),
        done as f64 / secs
    );
    Ok((done as u64, secs))
}

#[allow(clippy::too_many_arguments)]
async fn fill_ping(
    store: &SqliteStore,
    layer: PingLayer,
    ids: &[i64],
    tasks: i64,
    start: i64,
    step: i64,
    points: i64,
    packets: i64,
    rng: &mut impl Rng,
) -> Result<u64> {
    let total = ids.len() as i64 * tasks * points;
    let t0 = Instant::now();
    let mut buf = Vec::with_capacity(BATCH);
    let mut done = 0i64;

    for p in 0..points {
        let ts = start + p * step;
        for &id in ids {
            for task in 1..=tasks {
                buf.push(gen_ping(rng, id, task, ts, packets));
                if buf.len() >= BATCH {
                    store.insert_ping(layer, &buf).await?;
                    done += buf.len() as i64;
                    buf.clear();
                    if done % 400_000 == 0 {
                        eprint!("\r  {}: {done}/{total} 行", layer.table());
                    }
                }
            }
        }
    }
    if !buf.is_empty() {
        store.insert_ping(layer, &buf).await?;
        done += buf.len() as i64;
    }
    let secs = t0.elapsed().as_secs_f64();
    eprintln!(
        "\r  {}: {done} 行，{secs:.1}s（{:.0} 行/秒）",
        layer.table(),
        done as f64 / secs
    );
    Ok(done as u64)
}

/// 跑一次并返回耗时毫秒。跑 `runs` 次取中位数，避免单次抖动。
async fn bench<F, Fut, T>(runs: usize, mut f: F) -> Result<(f64, T)>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut times = Vec::with_capacity(runs);
    let mut last = None;
    for _ in 0..runs {
        let t0 = Instant::now();
        last = Some(f().await?);
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Ok((times[times.len() / 2], last.unwrap()))
}

fn verdict(name: &str, actual: f64, budget: f64, unit: &str) -> bool {
    let ok = actual <= budget;
    println!(
        "  {} {:<34} {:>10.1} {unit}  (预算 ≤ {budget:.0})",
        if ok { "✔" } else { "✘" },
        name,
        actual
    );
    ok
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse()?;

    if let Some(p) = args.db_path() {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{p}{suffix}"));
        }
        if let Some(dir) = std::path::Path::new(p).parent() {
            std::fs::create_dir_all(dir).ok();
        }
    }

    println!("═══════════════════════════════════════════════════════════════");
    println!("  Pulse M1 容量验证 —— 承重假设 A1");
    println!("═══════════════════════════════════════════════════════════════");
    println!(
        "  规模: {} 台 · 指标保留 {} 天 · {} 个延迟目标 · 探测间隔 {}s",
        args.servers, args.days, args.ping_tasks, args.ping_interval_s
    );
    println!("  数据库: {}\n", args.db);

    let store = SqliteStore::open(&args.db).await.context("打开数据库")?;
    // 固定种子：同样的参数应当得到同样的数据，测量结果才可复现
    let mut rng = rand::rngs::StdRng::seed_from_u64(0xA2_1234_5678);

    let mut ids = Vec::with_capacity(args.servers as usize);
    for i in 0..args.servers {
        ids.push(
            store
                .create_server(&format!("node-{i:05}"), &format!("bench-token-{i:05}"), 0)
                .await?
                .id,
        );
    }

    // 时间轴取整点对齐，让上卷与查询的边界条件符合真实情况
    let now = 1_800_000_000i64 / 3600 * 3600;
    let policy = RetentionPolicy::default();

    println!("── 按稳态体量写入 ──────────────────────────────────────────────");
    // 每一层都直接按它在稳态下应有的体量写入。
    // 不能只写 raw 再靠 rollup 生成上层：那需要先造出 30 天的原始行
    // （5184 万行 / 3.86 GiB），而这正是分层方案要避免的东西。
    let (_, _) = fill_metrics(
        &store,
        MetricLayer::Minute,
        &ids,
        now - args.days * 86_400,
        60,
        args.days * 1440,
        &mut rng,
    )
    .await?;
    fill_metrics(
        &store,
        MetricLayer::Hour,
        &ids,
        now - policy.hour_days * 86_400,
        3600,
        policy.hour_days * 24,
        &mut rng,
    )
    .await?;

    let per_hour = 3600 / args.ping_interval_s;
    fill_ping(
        &store,
        PingLayer::Raw,
        &ids,
        args.ping_tasks,
        now - policy.ping_raw_hours * 3600,
        args.ping_interval_s,
        policy.ping_raw_hours * per_hour,
        3,
        &mut rng,
    )
    .await?;
    fill_ping(
        &store,
        PingLayer::M5,
        &ids,
        args.ping_tasks,
        now - policy.ping_5m_days * 86_400,
        300,
        policy.ping_5m_days * 288,
        3,
        &mut rng,
    )
    .await?;
    fill_ping(
        &store,
        PingLayer::Hour,
        &ids,
        args.ping_tasks,
        now - policy.ping_hour_days * 86_400,
        3600,
        policy.ping_hour_days * 24,
        3,
        &mut rng,
    )
    .await?;

    // 额外造一批已过期的分钟数据。
    // 不造的话 prune 会删 0 行 —— 那条「单块 ≤ 200ms」的预算就等于没测。
    let expired_days = 2;
    let expired_rows = expired_days * 1440;
    println!("  （另造 {} 天已过期数据供 prune 实测）", expired_days);
    fill_metrics(
        &store,
        MetricLayer::Minute,
        &ids,
        now - (policy.minute_days + expired_days) * 86_400,
        60,
        expired_rows,
        &mut rng,
    )
    .await?;

    // ── 体积 ──
    println!("\n── 稳态体积 ────────────────────────────────────────────────────");
    let st = store.stats().await?;
    let file_bytes = args
        .db_path()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len() as f64)
        .unwrap_or(st.size_bytes as f64);
    for (t, n) in &st.rows {
        println!("  {t:<16} {n:>12} 行");
    }
    println!(
        "  {:<16} {:>12.0} MiB (page_count × page_size)",
        "逻辑体积",
        st.size_bytes as f64 / MIB
    );
    println!(
        "  {:<16} {:>12.0} MiB (磁盘文件)",
        "实际文件",
        file_bytes / MIB
    );

    let m_rows = st.rows["metrics_minute"] + st.rows["metrics_hour"];
    let p_rows = st.rows["ping_raw"] + st.rows["ping_5m"] + st.rows["ping_hour"];
    println!(
        "  平均行宽（含索引）: 全库 {:.0} B/行",
        st.size_bytes as f64 / (m_rows + p_rows) as f64
    );

    // ── 查询延迟 ──
    println!("\n── 查询延迟（中位数 / 9 次）────────────────────────────────────");
    let probe = ids[ids.len() / 2];
    let (q6h, s6h) = bench(9, || async {
        Ok(store.query_metrics(probe, now - 21_600, now).await?)
    })
    .await?;
    let (q7d, s7d) = bench(9, || async {
        Ok(store.query_metrics(probe, now - 604_800, now).await?)
    })
    .await?;
    let (q30d, s30d) = bench(9, || async {
        Ok(store.query_metrics(probe, now - 2_592_000, now).await?)
    })
    .await?;
    let (q1y, s1y) = bench(9, || async {
        Ok(store.query_metrics(probe, now - 31_536_000, now).await?)
    })
    .await?;
    let (qping, sping) = bench(9, || async {
        Ok(store.query_ping(probe, 1, now - 2_592_000, now).await?)
    })
    .await?;
    println!("  单机 6h  曲线: {q6h:>7.2} ms  ({} 点)", s6h.len());
    println!("  单机 7d  曲线: {q7d:>7.2} ms  ({} 点)", s7d.len());
    println!("  单机 30d 曲线: {q30d:>7.2} ms  ({} 点)", s30d.len());
    println!("  单机 1y  曲线: {q1y:>7.2} ms  ({} 点)", s1y.len());
    println!("  单机 30d 延迟: {qping:>7.2} ms  ({} 点)", sping.len());

    // ── 写入与维护 ──
    println!("\n── 写入与维护 ──────────────────────────────────────────────────");
    let flush_rows: Vec<_> = ids
        .iter()
        .map(|&id| gen_metric(&mut rng, id, now + 60))
        .collect();
    let store_ref = &store;
    let (flush_ms, _) = bench(9, || {
        let rows = flush_rows.clone();
        async move { Ok(store_ref.insert_metrics(MetricLayer::Minute, &rows).await?) }
    })
    .await?;
    println!("  flush_minute（{} 行一事务）: {flush_ms:.2} ms", ids.len());

    // 单轮窗口有上限，落后时靠多轮追平。这里测的是**单轮**耗时 ——
    // 它决定写连接被独占多久，也就是 agent 上报被堵多久。
    let t_all = Instant::now();
    let (mut rounds, mut worst_ms, mut total_rows) = (0u32, 0f64, 0u64);
    loop {
        let t0 = Instant::now();
        let r = store.rollup(now).await?;
        worst_ms = worst_ms.max(t0.elapsed().as_secs_f64() * 1000.0);
        total_rows += r.metrics_hour + r.ping_5m + r.ping_hour;
        rounds += 1;
        if r.caught_up || rounds > 500 {
            break;
        }
    }
    println!(
        "  rollup 追平积压: {rounds} 轮，共 {:.1}s，{total_rows} 行；单轮最长 {worst_ms:.0} ms",
        t_all.elapsed().as_secs_f64()
    );

    let t0 = Instant::now();
    let prune = store.prune(&policy, now).await?;
    let pruned_total = prune.metrics_minute
        + prune.metrics_hour
        + prune.ping_raw
        + prune.ping_5m
        + prune.ping_hour;
    if pruned_total == 0 {
        println!("  ⚠ prune 没有删到任何行 —— 本次「单块耗时」的测量无效");
    }
    println!(
        "  prune 一轮: {:.0} ms，{} 块，单块最长 {} ms，删除 {} 行",
        t0.elapsed().as_secs_f64() * 1000.0,
        prune.chunks,
        prune.max_chunk_ms,
        pruned_total
    );

    // ── 对照预算 ──
    println!("\n── 对照 的预算 ───────────────────────────");
    let mut all_ok = true;
    let mm_mib = st.rows["metrics_minute"] as f64 * 180.0 / MIB; // 按测算行宽折算，仅供参考
    let _ = mm_mib;
    all_ok &= verdict("总体积", file_bytes / MIB, 1200.0, "MiB");
    all_ok &= verdict("单机 6h 曲线", q6h, 10.0, "ms");
    all_ok &= verdict("单机 30d 曲线", q30d, 10.0, "ms");
    // 7d 是分钟层的最坏情况（要扫 7×1440 = 10080 行），也是全局最慢的查询
    all_ok &= verdict("单机 7d 曲线（最坏）", q7d, 30.0, "ms");
    all_ok &= verdict("单机 30d 延迟", qping, 20.0, "ms");
    all_ok &= verdict("flush_minute", flush_ms, 20.0, "ms");
    all_ok &= verdict("prune 单块", prune.max_chunk_ms as f64, 200.0, "ms");
    // 这一条决定 agent 上报会被后台任务堵多久
    all_ok &= verdict("rollup 单轮（堵塞写连接）", worst_ms, 2000.0, "ms");
    let max_points = [s6h.len(), s7d.len(), s30d.len(), s1y.len(), sping.len()]
        .into_iter()
        .max()
        .unwrap_or(0);
    all_ok &= verdict("任意跨度返回点数", max_points as f64, 750.0, "点");

    println!("\n═══════════════════════════════════════════════════════════════");
    if all_ok {
        println!("  ✔ 全部达标 —— 承重假设 A1 在本机成立");
    } else {
        println!("  ✘ 有项目未达标 —— 按 回到架构层重新设计，");
        println!("    而不是在此基础上继续堆功能");
    }
    println!("═══════════════════════════════════════════════════════════════");

    if !args.keep {
        if let Some(p) = args.db_path() {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{p}{suffix}"));
            }
            println!("\n（测试库已删除，加 --keep 可保留）");
        }
    }

    if !all_ok {
        std::process::exit(1);
    }
    Ok(())
}
