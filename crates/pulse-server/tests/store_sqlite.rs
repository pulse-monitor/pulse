//! SQLite 存储层的集成测试。
//!
//! 这些测试跑**真实数据库**。上卷、分块删除、按跨度选层这些逻辑都在 SQL 里，
//! 单元测试碰不到 —— 只有真跑一遍才知道 SQL 是不是对的。

use pulse_server::store::{
    sqlite::SqliteStore, MetricLayer, MetricRow, PingLayer, PingRow, RetentionPolicy, ServerFacts,
    ServerId, ServerPatch, Storage,
};

const MIN: i64 = 60;
const HOUR: i64 = 3600;
const DAY: i64 = 86_400;

async fn store() -> (SqliteStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("建临时目录");
    let url = format!("sqlite://{}/t.db", dir.path().display());
    let s = SqliteStore::open(&url).await.expect("打开数据库");
    (s, dir)
}

/// 建一台测试服务器，返回它的 id。
async fn server(s: &SqliteStore, name: &str) -> ServerId {
    s.create_server(name, &format!("hash-{name}"), 0)
        .await
        .unwrap()
        .id
}

fn metric(server_id: i64, ts: i64, cpu: i64) -> MetricRow {
    MetricRow {
        server_id,
        ts,
        cpu_pct: Some(cpu),
        cpu_pct_max: Some(cpu + 100),
        mem_total: Some(1 << 30),
        mem_available: Some(1 << 29),
        disk_used: Some(1 << 33),
        disk_total: Some(1 << 34),
        net_in_speed: Some(1000),
        net_out_speed: Some(2000),
        net_in_total: Some(ts * 1000),
        ..Default::default()
    }
}

#[tokio::test]
async fn create_server_assigns_unique_uuid_and_incrementing_order() {
    let (s, _d) = store().await;
    let a = s.create_server("alpha", "hash-a", 100).await.unwrap();
    let b = s.create_server("beta", "hash-b", 100).await.unwrap();

    assert_ne!(a.id, b.id);
    assert_ne!(a.uuid, b.uuid, "uuid 必须唯一");
    assert!(!a.uuid.is_empty());
    // uuid 对外暴露，不能是自增序号 —— 那会泄露「你一共有几台机器」
    assert!(a.uuid.parse::<i64>().is_err(), "uuid 不能是纯数字");
    assert!(b.sort_order > a.sort_order, "新建的机器应排在后面");
    assert!(!a.hidden);
}

#[tokio::test]
async fn 按_uuid_查得到从没连过探针的机器() {
    // 详情页的 metrics / ping 接口靠这个回落。
    //
    // 原本它们只查内存里的 AppState，而那里只装**本进程见过的会话**：
    // 面板一重启，所有机器的详情页都 404 到探针重连为止；
    // 从没装过探针的机器则永远打不开 —— 哪怕账单、备注、历史指标都在库里。
    // 真机上点开卡片才发现的（metrics 和 ping 全是 404）。
    let (s, _d) = store().await;
    let created = s.create_server("从没连过", "hash", 0).await.unwrap();

    let found = s.get_server_by_uuid(&created.uuid).await.unwrap();
    assert_eq!(found.map(|x| x.id), Some(created.id));
    assert!(s
        .get_server_by_uuid("不存在的-uuid")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn agent_authenticates_by_token_hash_only() {
    let (s, _d) = store().await;
    let created = s.create_server("node", "sha256-of-token", 0).await.unwrap();

    let found = s.get_server_by_token("sha256-of-token").await.unwrap();
    assert_eq!(found.map(|x| x.id), Some(created.id));

    // 未知凭据查不到 —— agent 那边就是 401
    assert!(s.get_server_by_token("wrong").await.unwrap().is_none());

    // 重置 token 后旧的立刻失效
    s.set_server_token(created.id, "new-hash").await.unwrap();
    assert!(s
        .get_server_by_token("sha256-of-token")
        .await
        .unwrap()
        .is_none());
    assert!(s.get_server_by_token("new-hash").await.unwrap().is_some());
}

#[tokio::test]
async fn patch_distinguishes_unset_from_clearing() {
    // ServerPatch 用嵌套 Option：None = 不改，Some(None) = 改成空
    let (s, _d) = store().await;
    let id = server(&s, "node").await;

    s.update_server(
        id,
        &ServerPatch {
            note: Some(Some("机房 A".into())),
            buy_url: Some(Some("https://buy".into())),
            hidden: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let r = s.get_server(id).await.unwrap().unwrap();
    assert_eq!(r.note.as_deref(), Some("机房 A"));
    assert!(r.hidden);

    // 只改 name，note 必须保持不动
    s.update_server(
        id,
        &ServerPatch {
            name: Some("renamed".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let r = s.get_server(id).await.unwrap().unwrap();
    assert_eq!(r.name, "renamed");
    assert_eq!(r.note.as_deref(), Some("机房 A"), "未提及的字段不能被清空");

    // 显式清空
    s.update_server(
        id,
        &ServerPatch {
            note: Some(None),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(s.get_server(id).await.unwrap().unwrap().note, None);
}

#[tokio::test]
async fn empty_patch_reports_no_change_instead_of_silently_succeeding() {
    let (s, _d) = store().await;
    let id = server(&s, "node").await;
    assert!(!s.update_server(id, &ServerPatch::default()).await.unwrap());
}

#[tokio::test]
async fn deleting_a_server_cascades_to_its_metrics() {
    // 不级联的话会留下永远查不到的孤儿数据，白占空间
    let (s, _d) = store().await;
    let id = server(&s, "node").await;
    s.insert_metrics(MetricLayer::Minute, &[metric(id, 60, 1)])
        .await
        .unwrap();
    s.insert_ping(
        PingLayer::Raw,
        &[PingRow {
            server_id: id,
            task_id: 1,
            ts: 60,
            rtt_avg: Some(1),
            rtt_min: Some(1),
            rtt_max: Some(1),
            sent: 1,
            recv: 1,
        }],
    )
    .await
    .unwrap();
    assert_eq!(s.stats().await.unwrap().rows["metrics_minute"], 1);

    assert!(s.delete_server(id).await.unwrap());
    let st = s.stats().await.unwrap();
    assert_eq!(st.rows["servers"], 0);
    assert_eq!(st.rows["metrics_minute"], 0, "指标应随服务器一并删除");
    assert_eq!(st.rows["ping_raw"], 0);

    assert!(
        !s.delete_server(id).await.unwrap(),
        "重复删除应返回 false 而不是报错"
    );
}

#[tokio::test]
async fn runtime_config_roundtrip_and_defaults() {
    let (s, _d) = store().await;
    let id = server(&s, "node").await;

    // 没有配置行时给协议默认值（含默认网卡黑名单）
    let d = s.get_runtime_config(id).await.unwrap();
    assert_eq!(d.interval_s, 2);
    assert!(d.net_exclude.contains(&"docker*".to_string()));

    let cfg = pulse_proto::RuntimeConfig {
        interval_s: 5,
        net_include: vec!["eth0".into()],
        net_exclude: vec!["veth*".into()],
        gpu_enabled: true,
        report_temps: false,
        ..Default::default()
    };
    s.set_runtime_config(id, &cfg, 100).await.unwrap();

    let got = s.get_runtime_config(id).await.unwrap();
    assert_eq!(got.interval_s, 5);
    assert_eq!(got.net_include, vec!["eth0"]);
    assert!(got.gpu_enabled);
    assert!(!got.report_temps);

    // 覆盖写
    s.set_runtime_config(
        id,
        &pulse_proto::RuntimeConfig {
            interval_s: 10,
            ..Default::default()
        },
        200,
    )
    .await
    .unwrap();
    assert_eq!(s.get_runtime_config(id).await.unwrap().interval_s, 10);
}

#[tokio::test]
async fn runtime_config_is_sanitized_on_read() {
    // 数据库里的值被手工改坏时，读出来也必须落在合法区间
    let (s, _d) = store().await;
    let id = server(&s, "node").await;
    s.set_runtime_config(
        id,
        &pulse_proto::RuntimeConfig {
            interval_s: 200,
            ..Default::default()
        },
        0,
    )
    .await
    .unwrap();
    assert_eq!(s.get_runtime_config(id).await.unwrap().interval_s, 60);
}

#[tokio::test]
async fn hello_facts_are_written_back() {
    let (s, _d) = store().await;
    let id = server(&s, "node").await;
    s.update_server_facts(
        id,
        &ServerFacts {
            os: Some("Debian 12".into()),
            arch: Some("x86_64".into()),
            virtualization: Some("kvm".into()),
            cpu_cores: Some(2),
            mem_total: Some(1 << 30),
            agent_version: Some("0.0.1".into()),
            capabilities: Some(r#"{"temperature":false}"#.into()),
            last_ip: Some("203.0.113.7".into()),
            ..Default::default()
        },
        12345,
    )
    .await
    .unwrap();

    let r = s.get_server(id).await.unwrap().unwrap();
    assert_eq!(r.os.as_deref(), Some("Debian 12"));
    assert_eq!(r.virtualization.as_deref(), Some("kvm"));
    assert_eq!(r.cpu_cores, Some(2));
    assert_eq!(r.last_seen_at, Some(12345));
    assert!(r.capabilities.unwrap().contains("temperature"));
}

#[tokio::test]
async fn admin_and_token_revocation() {
    let (s, _d) = store().await;
    assert_eq!(s.count_admins().await.unwrap(), 0);

    let id = s
        .create_admin("admin", "$argon2id$fake", 100)
        .await
        .unwrap();
    assert_eq!(s.count_admins().await.unwrap(), 1);
    let u = s.get_admin("admin").await.unwrap().unwrap();
    assert_eq!(u.id, id);
    assert_eq!(u.password_hash, "$argon2id$fake");
    assert!(s.get_admin("nobody").await.unwrap().is_none());

    // 吊销：幂等，且只按哈希匹配
    assert!(!s.is_revoked("h1").await.unwrap());
    s.revoke_token("h1", 500).await.unwrap();
    s.revoke_token("h1", 500).await.unwrap();
    assert!(s.is_revoked("h1").await.unwrap());
    assert!(!s.is_revoked("h2").await.unwrap());

    // 过了原始有效期就可以清掉 —— 否则这张表无界增长
    s.revoke_token("h3", 100).await.unwrap();
    assert_eq!(s.sweep_revoked(300).await.unwrap(), 1);
    assert!(!s.is_revoked("h3").await.unwrap());
    assert!(s.is_revoked("h1").await.unwrap(), "还没到期的不能被清掉");
}

#[tokio::test]
async fn reorder_assigns_sequential_positions() {
    let (s, _d) = store().await;
    let a = server(&s, "a").await;
    let b = server(&s, "b").await;
    let c = server(&s, "c").await;

    s.reorder_servers(&[(c, 0), (a, 1), (b, 2)]).await.unwrap();
    let names: Vec<_> = s
        .list_servers()
        .await
        .unwrap()
        .into_iter()
        .map(|x| x.name)
        .collect();
    assert_eq!(names, vec!["c", "a", "b"]);
}

#[tokio::test]
async fn settings_roundtrip() {
    let (s, _d) = store().await;
    assert!(s.get_setting("k").await.unwrap().is_none());
    s.set_setting("k", "v1", 100).await.unwrap();
    assert_eq!(s.get_setting("k").await.unwrap().as_deref(), Some("v1"));
    s.set_setting("k", "v2", 200).await.unwrap();
    assert_eq!(s.get_setting("k").await.unwrap().as_deref(), Some("v2"));
}

#[tokio::test]
async fn insert_then_query_roundtrip() {
    let (s, _d) = store().await;
    let id = server(&s, "n").await;

    let base = 1_700_000_000 / MIN * MIN;
    let rows: Vec<_> = (0..60)
        .map(|i| metric(id, base + i * MIN, 1000 + i))
        .collect();
    assert_eq!(
        s.insert_metrics(MetricLayer::Minute, &rows).await.unwrap(),
        60
    );

    let series = s.query_metrics(id, base, base + 60 * MIN).await.unwrap();
    assert_eq!(series.len(), 60);
    assert_eq!(series.step, 60, "1 小时跨度不应抽稀");
    assert_eq!(series.cpu_pct[0], Some(1000));
    assert_eq!(series.cpu_pct[59], Some(1059));
    assert_eq!(series.mem_total[0], Some(1 << 30));
}

#[tokio::test]
async fn insert_is_idempotent_on_conflict() {
    // 补报场景：agent 重连后把断线期间的数据重发一遍，不能报错也不能翻倍
    let (s, _d) = store().await;
    let id = server(&s, "n").await;
    let rows = vec![metric(id, 600, 1111)];

    s.insert_metrics(MetricLayer::Minute, &rows).await.unwrap();
    s.insert_metrics(MetricLayer::Minute, &rows).await.unwrap();

    let n = s.stats().await.unwrap().rows["metrics_minute"];
    assert_eq!(n, 1, "重复插入同一 (server_id, ts) 必须覆盖而不是新增");

    // 覆盖时用新值
    s.insert_metrics(MetricLayer::Minute, &[metric(id, 600, 2222)])
        .await
        .unwrap();
    let series = s.query_metrics(id, 0, 3600).await.unwrap();
    assert_eq!(series.cpu_pct[0], Some(2222));
}

#[tokio::test]
async fn empty_insert_is_a_noop() {
    let (s, _d) = store().await;
    assert_eq!(s.insert_metrics(MetricLayer::Minute, &[]).await.unwrap(), 0);
    assert_eq!(s.insert_ping(PingLayer::Raw, &[]).await.unwrap(), 0);
}

#[tokio::test]
async fn query_downsamples_long_ranges_to_bounded_points() {
    let (s, _d) = store().await;
    let id = server(&s, "n").await;

    // 塞 3 天的分钟数据
    let base = 1_700_000_000 / MIN * MIN;
    let rows: Vec<_> = (0..(3 * 24 * 60))
        .map(|i| metric(id, base + i * MIN, 1000))
        .collect();
    s.insert_metrics(MetricLayer::Minute, &rows).await.unwrap();

    let series = s.query_metrics(id, base, base + 3 * DAY).await.unwrap();
    assert!(series.len() <= 750, "实际 {} 点，超过上限", series.len());
    assert!(series.len() > 500, "抽稀过头了，只剩 {} 点", series.len());
    assert_eq!(series.step % 60, 0, "step 必须是分钟层步长的整数倍");
    // 值恒定，抽稀后应保持
    assert_eq!(series.cpu_pct[0], Some(1000));
}

#[tokio::test]
async fn rollup_minute_to_hour_uses_avg_and_max_correctly() {
    let (s, _d) = store().await;
    let id = server(&s, "n").await;

    // 造一个完整小时：60 个点，cpu 从 0 到 5900，平均应为 2950
    let h = 1_700_000_000 / HOUR * HOUR;
    let rows: Vec<_> = (0..60).map(|i| metric(id, h + i * MIN, i * 100)).collect();
    s.insert_metrics(MetricLayer::Minute, &rows).await.unwrap();

    // now 落在该小时之后，rollup 才会处理这个已完成的小时
    let rep = s.rollup(h + HOUR + 60).await.unwrap();
    assert!(rep.metrics_hour > 0, "应上卷出小时行");

    // 查一个 > 7 天的跨度强制走小时层
    let series = s.query_metrics(id, h - 30 * DAY, h + HOUR).await.unwrap();
    let vals: Vec<_> = series.cpu_pct.iter().flatten().copied().collect();
    assert_eq!(vals, vec![2950], "小时层 cpu_pct 应为该小时 60 个点的平均");
}

#[tokio::test]
async fn rollup_is_idempotent() {
    let (s, _d) = store().await;
    let id = server(&s, "n").await;
    let h = 1_700_000_000 / HOUR * HOUR;
    s.insert_metrics(MetricLayer::Minute, &[metric(id, h, 500)])
        .await
        .unwrap();

    s.rollup(h + HOUR + 60).await.unwrap();
    let after_first = s.stats().await.unwrap().rows["metrics_hour"];
    s.rollup(h + HOUR + 60).await.unwrap();
    let after_second = s.stats().await.unwrap().rows["metrics_hour"];
    assert_eq!(after_first, after_second, "重复上卷不能产生重复行");
}

#[tokio::test]
async fn ping_rollup_weights_rtt_by_received_packets() {
    // 这是最容易写错的一处：不同样本数的平均值直接再平均是错的。
    // 桶 A：recv=1, rtt=1000    桶 B：recv=9, rtt=2000
    // 正确的加权平均 = (1000*1 + 2000*9) / 10 = 1900
    // 错误的算术平均 = (1000 + 2000) / 2 = 1500
    let (s, _d) = store().await;
    let id = server(&s, "n").await;
    let t = 1_700_000_000 / 300 * 300;

    s.insert_ping(
        PingLayer::Raw,
        &[
            PingRow {
                server_id: id,
                task_id: 1,
                ts: t,
                rtt_avg: Some(1000),
                rtt_min: Some(900),
                rtt_max: Some(1100),
                sent: 3,
                recv: 1,
            },
            PingRow {
                server_id: id,
                task_id: 1,
                ts: t + 60,
                rtt_avg: Some(2000),
                rtt_min: Some(1900),
                rtt_max: Some(2100),
                sent: 9,
                recv: 9,
            },
        ],
    )
    .await
    .unwrap();

    s.rollup(t + 600).await.unwrap();

    // 6h < range ≤ 7d 走 ping_5m 层
    let series = s.query_ping(id, 1, t - DAY, t + 300).await.unwrap();
    let rtt: Vec<_> = series.rtt_avg.iter().flatten().copied().collect();
    assert_eq!(rtt, vec![1900], "rtt_avg 必须按 recv 加权，不能算术平均");

    let loss: Vec<_> = series.loss_pct.iter().flatten().copied().collect();
    // sent=12, recv=10 → 丢包 2/12 = 16.67%（真百分比，不是 ×100 的整数）
    assert_eq!(loss.len(), 1);
    assert!(
        (loss[0] - 16.666).abs() < 0.01,
        "丢包率应由 sent/recv 计数现算，且是真百分比：{}",
        loss[0]
    );
}

#[tokio::test]
async fn ping_rollup_survives_total_packet_loss() {
    // recv 全为 0 时 SUM(recv)=0，NULLIF 必须挡住除零
    let (s, _d) = store().await;
    let id = server(&s, "n").await;
    let t = 1_700_000_000 / 300 * 300;

    s.insert_ping(
        PingLayer::Raw,
        &[PingRow {
            server_id: id,
            task_id: 1,
            ts: t,
            rtt_avg: None,
            rtt_min: None,
            rtt_max: None,
            sent: 3,
            recv: 0,
        }],
    )
    .await
    .unwrap();

    s.rollup(t + 600).await.unwrap();
    let series = s.query_ping(id, 1, t - DAY, t + 300).await.unwrap();
    assert_eq!(
        series.rtt_avg[0], None,
        "全丢包时 rtt 应为 NULL 而不是 0 或崩溃"
    );
    assert_eq!(
        series.loss_pct[0],
        Some(100.0),
        "全丢包就是 100%。这里曾经是 10000（百分比 ×100），\
         而实时那条路一直是真百分比 —— 详情页的丢包图因此把 y 轴画到了 10000"
    );
}

#[tokio::test]
async fn raw_ping_同一分钟分两批写入要累加而不是覆盖() {
    // agent 每 30 秒 flush 一次，探测间隔 < 60 秒时同一分钟会分两批到。
    // 原先 ON CONFLICT 是覆盖：后一批把前一批整个抹掉
    let (s, _d) = store().await;
    let id = server(&s, "n").await;
    let t = 1_700_000_000 / 60 * 60;
    let row = |rtt: Option<i64>, sent, recv| PingRow {
        server_id: id,
        task_id: 1,
        ts: t,
        rtt_avg: rtt,
        rtt_min: rtt.map(|v| v - 100),
        rtt_max: rtt.map(|v| v + 100),
        sent,
        recv,
    };

    s.insert_ping(PingLayer::Raw, &[row(Some(1000), 3, 3)])
        .await
        .unwrap();
    s.insert_ping(PingLayer::Raw, &[row(Some(3000), 3, 1)])
        .await
        .unwrap();
    // 第三批全丢包：rtt 为 NULL，不能把已有的 min/max 冲成 NULL
    s.insert_ping(PingLayer::Raw, &[row(None, 3, 0)])
        .await
        .unwrap();

    let series = s.query_ping(id, 1, t - 60, t + 60).await.unwrap();
    assert_eq!(series.ts, vec![t]);
    let loss = series.loss_pct[0].unwrap();
    assert!(
        (loss - 5.0 * 100.0 / 9.0).abs() < 0.01,
        "9 个包收到 4 个，丢包应为 55.6%，实际 {loss}"
    );
    assert_eq!(
        series.rtt_avg[0],
        Some(1500),
        "rtt 按 recv 加权：(1000×3 + 3000×1) / 4"
    );
    assert_eq!(series.rtt_min[0], Some(900));
    assert_eq!(series.rtt_max[0], Some(3100));
}

#[tokio::test]
async fn prune_deletes_only_expired_rows() {
    let (s, _d) = store().await;
    let id = server(&s, "n").await;
    let now = 100 * DAY;

    let policy = RetentionPolicy::default(); // minute 7 天
    let old = now - policy.minute_days * DAY - HOUR; // 过期
    let fresh = now - HOUR; // 保留

    s.insert_metrics(
        MetricLayer::Minute,
        &[metric(id, old, 1), metric(id, fresh, 2)],
    )
    .await
    .unwrap();
    assert_eq!(s.stats().await.unwrap().rows["metrics_minute"], 2);

    let rep = s.prune(&policy, now).await.unwrap();
    assert_eq!(rep.metrics_minute, 1, "只应删掉过期的那一行");
    assert_eq!(s.stats().await.unwrap().rows["metrics_minute"], 1);

    let series = s.query_metrics(id, now - 2 * HOUR, now).await.unwrap();
    assert_eq!(series.cpu_pct[0], Some(2), "保留的应当是新的那行");
}

#[tokio::test]
async fn prune_on_empty_db_is_safe_and_terminates() {
    // 失控防护：空表上的分块删除必须立刻收敛，不能空转
    let (s, _d) = store().await;
    let rep = s
        .prune(&RetentionPolicy::default(), 100 * DAY)
        .await
        .unwrap();
    assert_eq!(rep.metrics_minute, 0);
    assert_eq!(rep.chunks, 5, "5 张表各一轮就该结束");
}

#[tokio::test]
async fn stats_reports_size_and_row_counts() {
    let (s, _d) = store().await;
    let id = server(&s, "n").await;
    s.insert_metrics(
        MetricLayer::Minute,
        &[metric(id, 60, 1), metric(id, 120, 2)],
    )
    .await
    .unwrap();

    let st = s.stats().await.unwrap();
    assert_eq!(st.driver, "sqlite");
    assert!(st.size_bytes > 0);
    assert_eq!(st.page_size * st.page_count, st.size_bytes);
    assert_eq!(st.rows["servers"], 1);
    assert_eq!(st.rows["metrics_minute"], 2);
    assert_eq!(st.rows["metrics_hour"], 0);
}

#[tokio::test]
async fn query_of_unknown_server_returns_empty_not_error() {
    let (s, _d) = store().await;
    let series = s.query_metrics(9999, 0, HOUR).await.unwrap();
    assert!(series.is_empty());
    let p = s.query_ping(9999, 1, 0, HOUR).await.unwrap();
    assert!(p.is_empty());
}

#[tokio::test]
async fn foreign_key_rejects_orphan_metrics() {
    // foreign_keys=ON 必须真的生效：指向不存在服务器的行应当被拒绝，
    // 否则 prune 删掉 servers 行之后会留下永远查不到的孤儿数据
    let (s, _d) = store().await;
    let r = s
        .insert_metrics(MetricLayer::Minute, &[metric(12345, 60, 1)])
        .await;
    assert!(r.is_err(), "外键未生效：孤儿行被接受了");
}

#[tokio::test]
async fn rollup_picks_up_data_older_than_one_bucket() {
    // 回归测试：水位线的初始默认值曾经是「end - 一个桶」，导致首次运行时
    // 比一个桶更早的数据永远不会被上卷，然后在源表保留期到点后被 prune
    // 删掉 —— 静默丢数据。场景：面板停机数小时后重启。
    let (s, _d) = store().await;
    let id = server(&s, "n").await;

    let h = 1_700_000_000 / HOUR * HOUR;
    // 5 小时前的数据，跨 3 个完整小时
    let rows: Vec<_> = (0..180).map(|i| metric(id, h + i * MIN, 1000)).collect();
    s.insert_metrics(MetricLayer::Minute, &rows).await.unwrap();

    // now 在 5 小时之后才第一次跑 rollup
    let rep = s.rollup(h + 5 * HOUR).await.unwrap();
    assert_eq!(rep.metrics_hour, 3, "3 个完整小时都应被上卷，一个都不能漏");

    let series = s
        .query_metrics(id, h - 30 * DAY, h + 5 * HOUR)
        .await
        .unwrap();
    let vals: Vec<_> = series.cpu_pct.iter().flatten().copied().collect();
    assert_eq!(vals.len(), 3, "小时层应有 3 个点");
    assert!(vals.iter().all(|&v| v == 1000));
}

#[tokio::test]
async fn rollup_caps_each_round_and_reports_backlog() {
    // 回归测试：曾经一次性上卷全部积压 —— 实测 58 万行 / 16 秒，
    // 期间独占写连接把 agent 上报堵死。现在窗口有上限，靠多轮追平。
    let (s, _d) = store().await;
    let id = server(&s, "n").await;

    let h = 1_700_000_000 / HOUR * HOUR;
    // 100 小时的积压，超过单轮 60 桶的上限
    let rows: Vec<_> = (0..(100 * 60))
        .map(|i| metric(id, h + i * MIN, 1000))
        .collect();
    s.insert_metrics(MetricLayer::Minute, &rows).await.unwrap();

    let now = h + 101 * HOUR;
    let first = s.rollup(now).await.unwrap();
    assert!(!first.caught_up, "积压超过单轮上限时必须报告未追平");
    assert_eq!(
        first.metrics_hour, 20,
        "单轮最多处理 ROLLUP_MAX_BUCKETS 个桶"
    );

    // 连续跑到追平
    let mut rounds = 1;
    loop {
        let r = s.rollup(now).await.unwrap();
        rounds += 1;
        if r.caught_up {
            break;
        }
        assert!(rounds < 20, "追赶轮数异常，可能没在推进水位线");
    }

    let n = s.stats().await.unwrap().rows["metrics_hour"];
    assert_eq!(n, 100, "追平后 100 个小时一个都不能少");
}

#[tokio::test]
async fn rollup_on_empty_db_reports_caught_up() {
    let (s, _d) = store().await;
    let r = s.rollup(1_700_000_000).await.unwrap();
    assert!(
        r.caught_up,
        "空库必须直接报告已追平，否则调用方会空转到轮数上限"
    );
    assert_eq!(r.metrics_hour, 0);
}

// ===========================================================================
// M5：账单 / 流量 / 套餐 / 分组 / 汇率
// ===========================================================================

use pulse_server::domain::billing::Cycle;
use pulse_server::domain::rate::{Rates, Source};
use pulse_server::store::{BillingInput, GroupRow, PlanRow, TrafficConfig, TrafficRow};

fn billing_input(price: f64, currency: &str, cycle: &str, expire: Option<i64>) -> BillingInput {
    BillingInput {
        price,
        currency: currency.into(),
        cycle: cycle.into(),
        custom_cycle_days: None,
        cycle_start_at: None,
        expire_at: expire,
        auto_renew: false,
        purchased_at: None,
        remark: None,
    }
}

#[tokio::test]
async fn billing_roundtrip_and_currency_is_normalised() {
    let (s, _d) = store().await;
    let id = server(&s, "node").await;
    assert!(s.get_billing(id).await.unwrap().is_none());

    s.set_billing(
        id,
        &billing_input(10.88, "usd", "annual", Some(1_800_000_000)),
        0,
    )
    .await
    .unwrap();

    let b = s.get_billing(id).await.unwrap().unwrap();
    assert_eq!(b.price, 10.88);
    assert_eq!(b.currency, "USD", "货币代码应当统一成大写");
    assert_eq!(b.cycle, Cycle::Annual);
    assert_eq!(b.expire_at, Some(1_800_000_000));

    // 覆盖写
    s.set_billing(id, &billing_input(5.0, "CNY", "monthly", None), 100)
        .await
        .unwrap();
    let b = s.get_billing(id).await.unwrap().unwrap();
    assert_eq!((b.price, b.cycle), (5.0, Cycle::Monthly));

    assert!(s.delete_billing(id).await.unwrap());
    assert!(s.get_billing(id).await.unwrap().is_none());
}

#[tokio::test]
async fn corrupt_cycle_in_db_falls_back_instead_of_losing_the_row() {
    // 数据被改坏时给一个可解释的结果，而不是让整台机器的账单消失
    let (s, _d) = store().await;
    let id = server(&s, "node").await;
    s.set_billing(id, &billing_input(9.9, "USD", "bogus-cycle", Some(1)), 0)
        .await
        .unwrap();
    let b = s.get_billing(id).await.unwrap().unwrap();
    assert_eq!(b.cycle, Cycle::Monthly, "非法周期应当回落 monthly");
    assert_eq!(b.price, 9.9, "其余字段不受影响");
}

/// 回归：`purchased_at` / `remark` 曾经**只写不读** —— SELECT 里没这两列，
/// 于是后台加载账单后原样保存一次，就把它们清空了。
/// 任何「输入接受但读不回来」的字段都是这种静默数据丢失。
#[tokio::test]
async fn billing_purchased_at_and_remark_survive_a_roundtrip() {
    let (s, _d) = store().await;
    let id = server(&s, "node").await;

    let mut b = billing_input(5.0, "USD", "monthly", Some(1_800_000_000));
    b.purchased_at = Some(1_700_000_000);
    b.remark = Some("双十一活动价".into());
    s.set_billing(id, &b, 0).await.unwrap();

    let got = s.get_billing(id).await.unwrap().unwrap();
    assert_eq!(got.purchased_at, Some(1_700_000_000), "购买日期读不回来");
    assert_eq!(got.remark.as_deref(), Some("双十一活动价"), "备注读不回来");

    // all_billing 走的是另一条 SELECT，同样要带上这两列
    let all = s.all_billing().await.unwrap();
    let e = all.iter().find(|e| e.id == id).unwrap();
    let ab = e.billing.as_ref().expect("这台机器有账单");
    assert_eq!(ab.purchased_at, Some(1_700_000_000));
    assert_eq!(ab.remark.as_deref(), Some("双十一活动价"));
}

#[tokio::test]
async fn all_billing_includes_servers_without_billing() {
    // 汇总要能如实报出「N 台未填价格」，所以没账单的机器也必须出现在结果里
    let (s, _d) = store().await;
    let a = server(&s, "has-billing").await;
    let _b = server(&s, "no-billing").await;
    s.set_billing(a, &billing_input(10.0, "USD", "monthly", Some(1)), 0)
        .await
        .unwrap();

    let all = s.all_billing().await.unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all.iter().filter(|e| e.billing.is_some()).count(), 1);
    assert_eq!(all.iter().filter(|e| e.billing.is_none()).count(), 1);
    // 身份信息必须来自数据库 —— 从没连过 agent 的机器也要能显示名字
    assert!(all.iter().all(|e| !e.name.is_empty() && !e.uuid.is_empty()));
}

#[tokio::test]
async fn traffic_roundtrip_and_archive_is_idempotent() {
    let (s, _d) = store().await;
    let id = server(&s, "node").await;

    let row = TrafficRow {
        server_id: id,
        period_start: 1000,
        in_bytes: 500,
        out_bytes: 700,
        last_raw_in: Some(9000),
        last_raw_out: Some(8000),
        alerted_pct: vec![80],
    };
    s.upsert_traffic(&row, 100).await.unwrap();

    let got = s.get_traffic(id).await.unwrap().unwrap();
    assert_eq!((got.in_bytes, got.out_bytes), (500, 700));
    assert_eq!(got.last_raw_in, Some(9000));
    assert_eq!(
        got.alerted_pct,
        vec![80],
        "已报过的告警档位要持久化，避免重复轰炸"
    );

    // 归档幂等：面板停机后补做时会重复执行同一个周期
    s.archive_traffic(id, 1000, 2000, 500, 700).await.unwrap();
    s.archive_traffic(id, 1000, 2000, 500, 700).await.unwrap();
    assert_eq!(
        s.stats().await.unwrap().rows["traffic_history"],
        1,
        "同一周期重复归档必须覆盖而不是新增"
    );

    // 不同周期是不同的行
    s.archive_traffic(id, 2000, 3000, 100, 200).await.unwrap();
    assert_eq!(s.stats().await.unwrap().rows["traffic_history"], 2);
}

#[tokio::test]
async fn traffic_config_defaults_and_roundtrip() {
    let (s, _d) = store().await;
    let id = server(&s, "node").await;

    let d = s.get_traffic_config(id).await.unwrap();
    assert_eq!(d.calc_mode, "sum");
    assert_eq!(d.reset_day, 1);
    assert_eq!(d.limit_bytes, None, "默认无限流量");
    assert_eq!(d.alert_pct, vec![80, 95, 100]);

    s.set_traffic_config(
        id,
        &TrafficConfig {
            limit_bytes: Some(1 << 40),
            calc_mode: "max".into(),
            reset_day: 15,
            timezone: Some("America/New_York".into()),
            alert_pct: vec![50, 90],
        },
    )
    .await
    .unwrap();

    let c = s.get_traffic_config(id).await.unwrap();
    assert_eq!(c.calc_mode, "max");
    assert_eq!(c.reset_day, 15);
    assert_eq!(c.timezone.as_deref(), Some("America/New_York"));
    assert_eq!(c.alert_pct, vec![50, 90]);
}

#[tokio::test]
async fn reset_day_is_clamped_on_read() {
    // 数据库被手工改成 99 时，读出来也必须落在合法区间
    let (s, _d) = store().await;
    let id = server(&s, "node").await;
    s.set_traffic_config(
        id,
        &TrafficConfig {
            reset_day: 99,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(s.get_traffic_config(id).await.unwrap().reset_day, 31);
}

#[tokio::test]
async fn manual_rate_survives_an_auto_refresh() {
    // 这是人工汇率存在的意义：源不支持 RUB，用户填了就该一直有效，
    // 不能被下一次自动拉取悄悄冲掉
    let (s, _d) = store().await;
    s.set_manual_rate("rub", 95.0, 100).await.unwrap();

    let mut auto = Rates::new(200, "2026-09-02".into());
    auto.insert("CNY", 6.7215, Source::Frankfurter);
    auto.insert("RUB", 1.0, Source::Frankfurter); // 假装源突然给了个值
    s.save_rates(&auto, 200).await.unwrap();

    let r = s.load_rates().await.unwrap();
    assert_eq!(r.get("RUB").unwrap().rate, 95.0, "人工汇率必须优先");
    assert_eq!(r.get("RUB").unwrap().source, Source::Manual);
    assert!((r.get("CNY").unwrap().rate - 6.7215).abs() < 1e-9);
    assert_eq!(r.as_of, "2026-09-02");

    // 删掉人工值之后才轮到自动值
    assert!(s.delete_manual_rate("RUB").await.unwrap());
    assert!(s.load_rates().await.unwrap().get("RUB").is_none());
    assert!(
        !s.delete_manual_rate("RUB").await.unwrap(),
        "重复删除返回 false"
    );
}

#[tokio::test]
async fn delete_manual_rate_does_not_touch_auto_rates() {
    let (s, _d) = store().await;
    let mut auto = Rates::new(100, "2026-09-02".into());
    auto.insert("CNY", 6.7215, Source::Frankfurter);
    s.save_rates(&auto, 100).await.unwrap();

    assert!(
        !s.delete_manual_rate("CNY").await.unwrap(),
        "自动汇率不该被人工删除接口动到"
    );
    assert!(s.load_rates().await.unwrap().get("CNY").is_some());
}

#[tokio::test]
async fn plans_and_groups_crud() {
    let (s, _d) = store().await;

    let p = s
        .create_plan(
            &PlanRow {
                id: 0,
                name: "RackNerd 1G".into(),
                provider: Some("RackNerd".into()),
                buy_url: Some("https://buy".into()),
                review_url: Some("https://review".into()),
                price: Some(10.88),
                currency: Some("USD".into()),
                cycle: Some("annual".into()),
                created_at: 0,
            },
            100,
        )
        .await
        .unwrap();
    assert!(p.id > 0);
    assert_eq!(s.list_plans().await.unwrap().len(), 1);
    assert!(s
        .update_plan(
            p.id,
            &PlanRow {
                name: "改名".into(),
                ..p.clone()
            }
        )
        .await
        .unwrap());
    assert_eq!(s.list_plans().await.unwrap()[0].name, "改名");
    assert!(s.delete_plan(p.id).await.unwrap());
    assert!(!s.delete_plan(p.id).await.unwrap());

    let g = s
        .create_group(
            &GroupRow {
                id: 0,
                name: "美西".into(),
                color: Some("#16a34a".into()),
                icon: None,
                sort_order: 0,
            },
            100,
        )
        .await
        .unwrap();
    assert!(g.id > 0);
    assert_eq!(s.list_groups().await.unwrap().len(), 1);
    assert!(s.delete_group(g.id).await.unwrap());
}

#[tokio::test]
async fn deleting_a_group_orphans_servers_instead_of_deleting_them() {
    // 外键是 ON DELETE SET NULL —— 删分组绝不能连带删掉机器
    let (s, _d) = store().await;
    let g = s
        .create_group(
            &GroupRow {
                id: 0,
                name: "组".into(),
                color: None,
                icon: None,
                sort_order: 0,
            },
            0,
        )
        .await
        .unwrap();
    let id = server(&s, "node").await;
    s.update_server(
        id,
        &ServerPatch {
            group_id: Some(Some(g.id)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        s.get_server(id).await.unwrap().unwrap().group_id,
        Some(g.id)
    );

    s.delete_group(g.id).await.unwrap();
    let r = s.get_server(id).await.unwrap().expect("机器不能被连带删除");
    assert_eq!(r.group_id, None, "应当变成「未分组」");
}

// ===========================================================================
// M6：通知渠道 / 规则 / 告警状态机
// ===========================================================================

use pulse_server::domain::alert::{AlertEvent, AlertState};
use pulse_server::notify::{telegram, ChannelConfig};
use pulse_server::store::{ChannelInput, RuleRow};

const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

/// 建一条最简规则，返回 id。告警记录对 rule_id 有外键约束。
async fn rule(s: &SqliteStore, name: &str) -> i64 {
    s.create_rule(
        &RuleRow {
            id: 0,
            name: name.into(),
            event_kinds: vec!["cpu_high".into()],
            channel_ids: vec![1],
            scope_kind: None,
            scope_ids: None,
            params: serde_json::Value::Null,
            duration_s: 0,
            cooldown_s: 0,
            notify_resolve: true,
            title_tpl: None,
            body_tpl: None,
            enabled: true,
        },
        0,
    )
    .await
    .unwrap()
}

fn tg_channel(name: &str, token: &str) -> ChannelInput {
    ChannelInput {
        name: name.into(),
        config: ChannelConfig::Telegram(telegram::Config {
            token: token.into(),
            chat_id: "-100999".into(),
            proxy: None,
        }),
        enabled: true,
    }
}

#[tokio::test]
async fn channel_credentials_are_encrypted_at_rest() {
    let (s, d) = store().await;
    let token = "1234567890:SUPER-SECRET-BOT-TOKEN";
    let id = s
        .create_channel(&tg_channel("tg", token), SECRET, 0)
        .await
        .unwrap();
    assert!(id > 0);

    // 读回来能正常解密
    let cs = s.list_channels(SECRET).await.unwrap();
    assert_eq!(cs.len(), 1);
    match &cs[0].config {
        ChannelConfig::Telegram(c) => assert_eq!(c.token, token),
        _ => panic!("渠道类型不对"),
    }

    // **数据库文件里不能出现明文** —— 这是加密存储的意义
    let raw = std::fs::read(d.path().join("t.db")).unwrap();
    let hay = String::from_utf8_lossy(&raw);
    assert!(
        !hay.contains("SUPER-SECRET-BOT-TOKEN"),
        "凭据明文出现在数据库文件里"
    );
}

#[tokio::test]
async fn channels_that_cannot_be_decrypted_are_skipped_not_fatal() {
    // 换过密钥时所有渠道都解不开。这时至少还能看到其他配置，
    // 而不是让整个列表接口失败
    let (s, _d) = store().await;
    s.create_channel(&tg_channel("tg", "1:ABC"), SECRET, 0)
        .await
        .unwrap();

    let wrong = b"ffffffffffffffffffffffffffffffff";
    let cs = s.list_channels(wrong).await.unwrap();
    assert!(
        cs.is_empty(),
        "解不开的渠道应当被跳过，而不是报错或返回垃圾"
    );
}

#[tokio::test]
async fn rules_roundtrip_with_scope_and_params() {
    let (s, _d) = store().await;
    let r = RuleRow {
        id: 0,
        name: "CPU 告警".into(),
        event_kinds: vec!["cpu_high".into(), "mem_high".into()],
        channel_ids: vec![1, 2],
        scope_kind: Some("servers".into()),
        scope_ids: Some(vec![7, 8]),
        params: serde_json::json!({ "cpu_pct": 85 }),
        duration_s: 300,
        cooldown_s: 1800,
        notify_resolve: false,
        title_tpl: Some("自定义 {{ server.name }}".into()),
        body_tpl: None,
        enabled: true,
    };
    let id = s.create_rule(&r, 100).await.unwrap();

    let got = &s.list_rules().await.unwrap()[0];
    assert_eq!(got.id, id);
    assert_eq!(got.event_kinds, vec!["cpu_high", "mem_high"]);
    assert_eq!(got.channel_ids, vec![1, 2]);
    assert_eq!(got.params["cpu_pct"], 85);
    assert_eq!(got.duration_s, 300);
    assert!(!got.notify_resolve);
    assert_eq!(got.title_tpl.as_deref(), Some("自定义 {{ server.name }}"));
    // 作用范围要能原样读回
    assert!(got.scope().covers(7, None));
    assert!(!got.scope().covers(9, None));

    assert!(s.delete_rule(id).await.unwrap());
    assert!(s.list_rules().await.unwrap().is_empty());
}

#[tokio::test]
async fn alert_state_survives_a_restart() {
    // **这是状态持久化存在的意义**：进程重启后从数据库读回 firing，
    // 冷却期内不能再发一条
    let (s, _d) = store().await;
    let sid = server(&s, "node").await;
    let rid = s
        .create_rule(
            &RuleRow {
                id: 0,
                name: "r".into(),
                event_kinds: vec!["cpu_high".into()],
                channel_ids: vec![1],
                scope_kind: None,
                scope_ids: None,
                params: serde_json::Value::Null,
                duration_s: 90,
                cooldown_s: 3600,
                notify_resolve: true,
                title_tpl: None,
                body_tpl: None,
                enabled: true,
            },
            0,
        )
        .await
        .unwrap();

    let firing = AlertEvent {
        state: AlertState::Firing,
        first_at: 1000,
        fired_at: Some(1090),
        resolved_at: None,
        last_notified_at: Some(1090),
    };
    s.upsert_alert(
        rid,
        Some(sid),
        "cpu_high",
        &firing,
        Some(r#"{"value":"95%"}"#),
    )
    .await
    .unwrap();

    // 「重启」= 重新读一次
    let back = s
        .get_alert(rid, Some(sid), "cpu_high")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(back, firing, "状态必须原样读回，否则重启就会重复轰炸");
}

#[tokio::test]
async fn only_one_active_alert_per_rule_server_kind() {
    let (s, _d) = store().await;
    let sid = server(&s, "node").await;
    let rid = rule(&s, "r").await;

    let mk = |state, first_at| AlertEvent {
        state,
        first_at,
        fired_at: None,
        resolved_at: None,
        last_notified_at: None,
    };
    // 反复 upsert 只应留下最后一条活跃记录
    for i in 0..5 {
        s.upsert_alert(
            rid,
            Some(sid),
            "cpu_high",
            &mk(AlertState::Pending, i),
            None,
        )
        .await
        .unwrap();
    }
    let a = s
        .get_alert(rid, Some(sid), "cpu_high")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(a.first_at, 4, "应当是最后一次写入的那条");

    // 不同事件类型互不影响
    s.upsert_alert(
        rid,
        Some(sid),
        "mem_high",
        &mk(AlertState::Firing, 99),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        s.get_alert(rid, Some(sid), "cpu_high")
            .await
            .unwrap()
            .unwrap()
            .first_at,
        4
    );
    assert_eq!(
        s.get_alert(rid, Some(sid), "mem_high")
            .await
            .unwrap()
            .unwrap()
            .first_at,
        99
    );

    // 清掉之后读不到
    s.clear_alert(rid, Some(sid), "cpu_high").await.unwrap();
    assert!(s
        .get_alert(rid, Some(sid), "cpu_high")
        .await
        .unwrap()
        .is_none());
    assert!(s
        .get_alert(rid, Some(sid), "mem_high")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn resolved_alerts_stay_in_history_but_are_not_active() {
    let (s, _d) = store().await;
    let sid = server(&s, "node").await;
    let rid = rule(&s, "r").await;
    let resolved = AlertEvent {
        state: AlertState::Resolved,
        first_at: 100,
        fired_at: Some(190),
        resolved_at: Some(500),
        last_notified_at: Some(190),
    };
    s.upsert_alert(rid, Some(sid), "cpu_high", &resolved, None)
        .await
        .unwrap();

    // 已恢复的不算活跃记录
    assert!(s
        .get_alert(rid, Some(sid), "cpu_high")
        .await
        .unwrap()
        .is_none());
    // 但要留在历史里
    let h = s.list_alerts(10).await.unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].state, "resolved");
    assert_eq!(h[0].resolved_at, Some(500));
}

#[tokio::test]
async fn deleting_a_rule_cascades_to_its_alerts() {
    let (s, _d) = store().await;
    let sid = server(&s, "node").await;
    let rid = s
        .create_rule(
            &RuleRow {
                id: 0,
                name: "r".into(),
                event_kinds: vec!["cpu_high".into()],
                channel_ids: vec![1],
                scope_kind: None,
                scope_ids: None,
                params: serde_json::Value::Null,
                duration_s: 0,
                cooldown_s: 0,
                notify_resolve: true,
                title_tpl: None,
                body_tpl: None,
                enabled: true,
            },
            0,
        )
        .await
        .unwrap();
    s.upsert_alert(
        rid,
        Some(sid),
        "cpu_high",
        &AlertEvent {
            state: AlertState::Firing,
            first_at: 0,
            fired_at: None,
            resolved_at: None,
            last_notified_at: None,
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(s.list_alerts(10).await.unwrap().len(), 1);

    s.delete_rule(rid).await.unwrap();
    assert!(
        s.list_alerts(10).await.unwrap().is_empty(),
        "规则删掉时告警记录应一并清理"
    );
}

// ---------------------------------------------------------------------------
// 首次初始化：create_first_admin 的原子性
// ---------------------------------------------------------------------------

#[tokio::test]
async fn first_admin_only_once() {
    let (s, _d) = store().await;
    assert_eq!(s.count_admins().await.unwrap(), 0);

    let id = s.create_first_admin("alice", "hash-a", 100).await.unwrap();
    assert!(id.is_some(), "第一个应该建得成");
    assert_eq!(s.count_admins().await.unwrap(), 1);

    // 换个用户名再来一次 —— 靠 UNIQUE(username) 是拦不住的，
    // 必须靠 WHERE NOT EXISTS
    let again = s.create_first_admin("bob", "hash-b", 200).await.unwrap();
    assert!(again.is_none(), "已经有管理员了，第二个必须被拒");
    assert_eq!(s.count_admins().await.unwrap(), 1);
    assert!(s.get_admin("bob").await.unwrap().is_none());

    let alice = s.get_admin("alice").await.unwrap().expect("alice 在");
    assert_eq!(alice.password_hash, "hash-a");
}

#[tokio::test]
async fn first_admin_blocked_by_existing_admin() {
    let (s, _d) = store().await;
    // 用无人值守那条路径（PULSE_ADMIN_PASSWORD）先建好
    s.create_admin("admin", "hash-x", 1).await.unwrap();

    let r = s.create_first_admin("attacker", "hash-y", 2).await.unwrap();
    assert!(r.is_none(), "预先建过管理员，初始化接口就该是关的");
    assert_eq!(s.count_admins().await.unwrap(), 1);
}

/// 并发调用只能有一个成功。
///
/// 这是这个方法存在的**唯一理由** —— 顺序调用用 count+insert 也能过，
/// 只有并发才能把 TOCTOU 暴露出来。
#[tokio::test]
async fn first_admin_concurrent_single_winner() {
    let dir = tempfile::tempdir().expect("建临时目录");
    let url = format!("sqlite://{}/t.db", dir.path().display());
    let s = std::sync::Arc::new(SqliteStore::open(&url).await.expect("打开数据库"));

    let mut set = tokio::task::JoinSet::new();
    for i in 0..8 {
        let s = s.clone();
        set.spawn(async move {
            s.create_first_admin(&format!("user{i}"), "h", 1)
                .await
                .unwrap()
                .is_some()
        });
    }
    let mut wins = 0;
    while let Some(r) = set.join_next().await {
        if r.unwrap() {
            wins += 1;
        }
    }
    assert_eq!(wins, 1, "并发 8 个只能有 1 个建成，实际 {wins}");
    assert_eq!(s.count_admins().await.unwrap(), 1);
}
