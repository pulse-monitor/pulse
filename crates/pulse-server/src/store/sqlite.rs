//! SQLite 后端。
//!
//! 关键配置全部来自 ，改动前请先读那一篇 ——
//! 这里的每条 PRAGMA、每个"批量"和"分块"都是为了避开一个具体的坑。

use std::collections::BTreeMap;
use std::str::FromStr;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use sqlx::sqlite::{
    SqliteAutoVacuum, SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::{Row, SqlitePool};
use tracing::{debug, info, warn};

use super::{
    metric_col_list, plan_query, AdminUser, AlertRow, BillingEntry, BillingInput, ChannelInput,
    ChannelRow, Granularity, GroupRow, MetricLayer, MetricRow, MetricSeries, PingLayer, PingRow,
    PingScope, PingSeries, PingTaskInput, PingTaskRow, PlanRow, PruneReport, Result,
    RetentionPolicy, RollupReport, RuleRow, ServerFacts, ServerId, ServerPatch, ServerRecord,
    Storage, StorageStats, TrafficConfig, TrafficRow, METRIC_COLS,
};
use crate::domain::alert::{AlertEvent, AlertState};
use crate::domain::billing::{Billing, Cycle};
use crate::domain::rate::{Rates, Source};
use crate::notify::crypto;

/// 一条 INSERT 里最多放多少行。
///
/// SQLite 的 `SQLITE_MAX_VARIABLE_NUMBER` 现代版本默认 32766。
/// 指标表 22 个可空列 + 2 个键 = 24 个占位符/行，800 行 = 19200，留足余量。
const METRIC_ROWS_PER_STMT: usize = 800;
const PING_ROWS_PER_STMT: usize = 2_000;

/// 单次分块删除的行数上限。
///
/// 一次删几百万行会让 WAL 暴涨、checkpoint 卡死、写者全部阻塞。
const PRUNE_CHUNK: i64 = 50_000;

/// 分块删除的总轮数上限 —— 失控防护，避免任何情况下变成无限循环。
const PRUNE_MAX_CHUNKS: u64 = 10_000;

/// 单轮上卷**每层**最多处理多少个目标桶。
///
/// 实测（crates/pulse-loadgen，200 台 / 6 目标）：
/// - 不限窗口：一次性卷 7 天积压 = 59 万行 / 16 秒，期间独占写连接
/// - 60 桶：单轮 15.6 万行 / 4.5 秒 —— 三层是在同一次调用里跑的，要按总和算
/// - 20 桶：单轮约 5.2 万行 / 1.5 秒 ✔
///
/// 追平总耗时由吞吐决定，与桶数无关（都是约 15 秒），所以缩小窗口
/// 只缩短单次锁持有时间，不牺牲追赶速度 —— 纯赚。
///
/// ⚠️ 每桶行数 = 机器数 × 延迟目标数，所以这个常量是按「≤200 台 × ≤6 目标」
/// 调的。规模再大时单轮会等比变长；届时应当切 PostgreSQL，
/// 它的分区表让上卷与清理是完全不同的量级。
const ROLLUP_MAX_BUCKETS: i64 = 20;

// ---------------------------------------------------------------------------

pub struct SqliteStore {
    /// 写池固定单连接：SQLite 同一时刻只允许一个写者，
    /// 多写连接只会制造 SQLITE_BUSY 重试，没有收益。
    write: SqlitePool,
    /// 读池多连接：WAL 下读不阻塞写。
    read: SqlitePool,
}

impl SqliteStore {
    /// `url` 形如 `sqlite://data/pulse.db`。
    pub async fn open(url: &str) -> Result<Self> {
        // 读写两个池的选项必须分开构造。
        // journal_mode / auto_vacuum 是**数据库的持久属性**，写者设一次即可；
        // 在只读连接上执行这两条 PRAGMA 会直接报 "attempt to write a readonly database"。
        let common = |u: &str| -> Result<SqliteConnectOptions> {
            Ok(SqliteConnectOptions::from_str(u)?
                .busy_timeout(Duration::from_secs(5))
                .foreign_keys(true) // 按连接生效，不写盘
                .pragma("cache_size", "-65536") // 64 MiB page cache
                .pragma("temp_store", "MEMORY")
                .pragma("mmap_size", "268435456")) // 256 MiB
        };

        let write_opts = common(url)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal) // 读写不互斥，并发的前提
            .synchronous(SqliteSynchronous::Normal) // WAL 下安全，最坏丢最后一个事务
            // 必须在建库时设置，之后改需要整库 VACUUM
            .auto_vacuum(SqliteAutoVacuum::Incremental)
            .pragma("wal_autocheckpoint", "2000"); // ~8 MiB

        // 写池固定单连接：SQLite 同一时刻只允许一个写者，
        // 多写连接只会制造 SQLITE_BUSY 重试，没有收益。
        let write = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(write_opts)
            .await?;

        sqlx::migrate!("./migrations/sqlite").run(&write).await?;

        // 读池多连接：WAL 下读不阻塞写。read_only 是纵深防御 ——
        // 读路径上写错任何一条 SQL 都会被数据库直接挡住。
        let readers = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(4)
            .clamp(2, 8);
        let read = SqlitePoolOptions::new()
            .max_connections(readers)
            .connect_with(common(url)?.read_only(true))
            .await?;

        info!(readers, "SQLite 已打开（写池 1 连接，读池 {readers} 连接）");
        Ok(Self { write, read })
    }

    /// 读取上卷水位线；没有记录时返回 `None`。
    async fn watermark(&self, name: &str) -> Result<Option<i64>> {
        let r = sqlx::query("SELECT ts FROM rollup_watermark WHERE name = ?")
            .bind(name)
            .fetch_optional(&self.read)
            .await?;
        Ok(r.map(|r| r.get::<i64, _>("ts")))
    }

    /// 源表里最早一行的时间戳。空表返回 `None`。
    async fn min_ts(&self, table: &str) -> Result<Option<i64>> {
        let r = sqlx::query(&format!("SELECT MIN(ts) AS m FROM {table}"))
            .fetch_one(&self.read)
            .await?;
        Ok(r.get::<Option<i64>, _>("m"))
    }

    /// 上卷的起点：优先用持久化的水位线，没有则从源表最早的数据开始。
    ///
    /// **不能默认成「end - 一个桶」**：那样首次运行时比一个桶更早的数据
    /// 永远不会被上卷，然后在源表保留期到点后被 prune 删掉 —— 静默丢数据。
    /// 源表本身有保留期（raw 24 小时、minute 7 天），所以回溯范围天然有界。
    async fn rollup_from(&self, watermark: &str, src: &str) -> Result<Option<i64>> {
        match self.watermark(watermark).await? {
            Some(w) => Ok(Some(w)),
            None => self.min_ts(src).await,
        }
    }

    async fn set_watermark(&self, name: &str, ts: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO rollup_watermark(name, ts) VALUES (?, ?)
             ON CONFLICT(name) DO UPDATE SET ts = excluded.ts",
        )
        .bind(name)
        .bind(ts)
        .execute(&self.write)
        .await?;
        Ok(())
    }

    /// 分块删除，返回 (删除行数, 块数, 单块最长耗时)。
    async fn prune_table(&self, table: &str, cutoff: i64) -> Result<(u64, u64, u128)> {
        // 表名不来自外部输入，是本模块的常量，不存在注入面
        let sql = format!(
            "DELETE FROM {table} WHERE rowid IN \
             (SELECT rowid FROM {table} WHERE ts < ? LIMIT ?)"
        );
        let (mut total, mut chunks, mut max_ms) = (0u64, 0u64, 0u128);
        loop {
            let t0 = Instant::now();
            let n = sqlx::query(&sql)
                .bind(cutoff)
                .bind(PRUNE_CHUNK)
                .execute(&self.write)
                .await?
                .rows_affected();
            max_ms = max_ms.max(t0.elapsed().as_millis());
            total += n;
            chunks += 1;

            if n < PRUNE_CHUNK as u64 {
                break;
            }
            if chunks >= PRUNE_MAX_CHUNKS {
                warn!(
                    table,
                    chunks, "分块删除达到轮数上限，本轮提前结束，下次继续"
                );
                break;
            }
            // 让出执行权，避免长时间独占写连接把上报流量堵住
            tokio::task::yield_now().await;
        }
        Ok((total, chunks, max_ms))
    }

    /// 把细粒度层上卷到粗粒度层。`src`/`dst` 是本模块常量。
    async fn rollup_metrics(&self, from: i64, to: i64) -> Result<u64> {
        let cols = metric_col_list();
        let aggs = METRIC_COLS
            .iter()
            .map(|(c, a)| a.sql(c))
            .collect::<Vec<_>>()
            .join(", ");
        let updates = METRIC_COLS
            .iter()
            .map(|(c, _)| format!("{c} = excluded.{c}"))
            .collect::<Vec<_>>()
            .join(", ");

        let sql = format!(
            "INSERT INTO metrics_hour (server_id, ts, {cols})
             SELECT server_id, (ts / 3600) * 3600 AS h, {aggs}
             FROM metrics_minute
             WHERE ts >= ? AND ts < ?
             GROUP BY server_id, h
             ON CONFLICT(server_id, ts) DO UPDATE SET {updates}"
        );
        Ok(sqlx::query(&sql)
            .bind(from)
            .bind(to)
            .execute(&self.write)
            .await?
            .rows_affected())
    }

    async fn rollup_ping(
        &self,
        src: &str,
        dst: &str,
        bucket: i64,
        from: i64,
        to: i64,
    ) -> Result<u64> {
        // rtt_avg 必须按 recv 加权：不同样本数的平均值直接再平均是错的。
        // recv 全为 0（全丢包）时 SUM 为 0，NULLIF 让结果变 NULL 而不是除零。
        let sql = format!(
            "INSERT INTO {dst} (server_id, task_id, ts, rtt_avg, rtt_min, rtt_max, sent, recv)
             SELECT server_id, task_id, (ts / ?) * ? AS b,
                    CAST(SUM(rtt_avg * recv) / NULLIF(SUM(recv), 0) AS INTEGER),
                    MIN(rtt_min), MAX(rtt_max), SUM(sent), SUM(recv)
             FROM {src}
             WHERE ts >= ? AND ts < ?
             GROUP BY server_id, task_id, b
             ON CONFLICT(server_id, task_id, ts) DO UPDATE SET
                rtt_avg = excluded.rtt_avg, rtt_min = excluded.rtt_min,
                rtt_max = excluded.rtt_max, sent = excluded.sent, recv = excluded.recv"
        );
        Ok(sqlx::query(&sql)
            .bind(bucket)
            .bind(bucket)
            .bind(from)
            .bind(to)
            .execute(&self.write)
            .await?
            .rows_affected())
    }
}

/// 服务器记录的字段清单。列顺序集中在这里，避免 SELECT 与取值两处各写一遍。
const SERVER_SELECT: &str = "SELECT id, uuid, name, group_id, sort_order, hidden, note,
        buy_url, review_url, country_code, region, latitude, longitude, location_manual,
        asn, isp, last_ip, os, kernel, arch, virtualization, cpu_model, cpu_cores,
        mem_total, swap_total, disk_total, agent_version, boot_at, capabilities,
        first_seen_at, last_seen_at
    FROM servers";

const PING_TASK_SELECT: &str = "SELECT id, name, kind, host, port, expect_status,
        interval_s, packets, timeout_ms, scope_kind, scope_ids, enabled, created_at
    FROM ping_tasks";

const RULE_SELECT: &str = "SELECT id, name, event_kinds, channel_ids, scope_kind, scope_ids,
        params, duration_s, cooldown_s, notify_resolve, title_tpl, body_tpl, enabled
    FROM notification_rules";

fn rule_from_row(r: &sqlx::sqlite::SqliteRow) -> RuleRow {
    // 泛型闭包在 Rust 里写不出来，分成两个
    let strs = |c: &str| -> Vec<String> {
        serde_json::from_str(&r.get::<String, _>(c)).unwrap_or_default()
    };
    let ints =
        |c: &str| -> Vec<i64> { serde_json::from_str(&r.get::<String, _>(c)).unwrap_or_default() };
    RuleRow {
        id: r.get("id"),
        name: r.get("name"),
        event_kinds: strs("event_kinds"),
        channel_ids: ints("channel_ids"),
        scope_kind: Some(r.get("scope_kind")),
        scope_ids: Some(ints("scope_ids")),
        params: serde_json::from_str(&r.get::<String, _>("params"))
            .unwrap_or(serde_json::Value::Null),
        duration_s: r.get("duration_s"),
        cooldown_s: r.get("cooldown_s"),
        notify_resolve: r.get::<i64, _>("notify_resolve") != 0,
        title_tpl: r.get("title_tpl"),
        body_tpl: r.get("body_tpl"),
        enabled: r.get::<i64, _>("enabled") != 0,
    }
}

fn json_or_empty<T: serde::Serialize>(v: &[T]) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".into())
}

/// 凭据加密。加密失败当成数据库错误往上抛 —— 绝不能明文落库。
fn encrypt_config(c: &crate::notify::ChannelConfig, secret: &[u8]) -> Result<Vec<u8>> {
    let json = serde_json::to_string(c).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    crypto::encrypt(secret, &json)
        .map_err(|e| sqlx::Error::Encode(Box::new(std::io::Error::other(e.to_string()))))
}

const PLAN_SELECT: &str =
    "SELECT id, name, provider, buy_url, review_url, price, currency, cycle, created_at FROM plans";

fn plan_from_row(r: &sqlx::sqlite::SqliteRow) -> PlanRow {
    PlanRow {
        id: r.get("id"),
        name: r.get("name"),
        provider: r.get("provider"),
        buy_url: r.get("buy_url"),
        review_url: r.get("review_url"),
        price: r.get("price"),
        currency: r.get("currency"),
        cycle: r.get("cycle"),
        created_at: r.get("created_at"),
    }
}

fn group_from_row(r: &sqlx::sqlite::SqliteRow) -> GroupRow {
    GroupRow {
        id: r.get("id"),
        name: r.get("name"),
        color: r.get("color"),
        icon: r.get("icon"),
        sort_order: r.get("sort_order"),
    }
}

fn billing_from_row(r: &sqlx::sqlite::SqliteRow) -> Billing {
    Billing {
        price: r.get::<Option<f64>, _>("price").unwrap_or(0.0),
        currency: r
            .get::<Option<String>, _>("currency")
            .unwrap_or_else(|| "USD".into()),
        // 非法周期回落 monthly：数据被改坏时给一个可解释的结果，
        // 而不是让整台机器的账单消失
        cycle: r
            .get::<Option<String>, _>("cycle")
            .as_deref()
            .and_then(Cycle::parse)
            .unwrap_or(Cycle::Monthly),
        custom_cycle_days: r.get("custom_cycle_days"),
        cycle_start_at: r.get("cycle_start_at"),
        expire_at: r.get("expire_at"),
        auto_renew: r.get::<Option<i64>, _>("auto_renew").unwrap_or(0) != 0,
        purchased_at: r.get("purchased_at"),
        remark: r.get("remark"),
    }
}

fn ping_task_from_row(r: &sqlx::sqlite::SqliteRow) -> PingTaskRow {
    PingTaskRow {
        id: r.get("id"),
        name: r.get("name"),
        kind: r.get("kind"),
        host: r.get("host"),
        port: r.get("port"),
        expect_status: r.get("expect_status"),
        interval_s: r.get("interval_s"),
        packets: r.get("packets"),
        timeout_ms: r.get("timeout_ms"),
        scope: PingScope::from_columns(
            &r.get::<String, _>("scope_kind"),
            &r.get::<String, _>("scope_ids"),
        ),
        enabled: r.get::<i64, _>("enabled") != 0,
        created_at: r.get("created_at"),
    }
}

fn server_from_row(r: &sqlx::sqlite::SqliteRow) -> ServerRecord {
    ServerRecord {
        id: r.get("id"),
        uuid: r.get("uuid"),
        name: r.get("name"),
        group_id: r.get("group_id"),
        sort_order: r.get("sort_order"),
        hidden: r.get::<i64, _>("hidden") != 0,
        note: r.get("note"),
        buy_url: r.get("buy_url"),
        review_url: r.get("review_url"),
        country_code: r.get("country_code"),
        region: r.get("region"),
        latitude: r.get("latitude"),
        longitude: r.get("longitude"),
        location_manual: r.get::<i64, _>("location_manual") != 0,
        asn: r.get("asn"),
        isp: r.get("isp"),
        last_ip: r.get("last_ip"),
        os: r.get("os"),
        kernel: r.get("kernel"),
        arch: r.get("arch"),
        virtualization: r.get("virtualization"),
        cpu_model: r.get("cpu_model"),
        cpu_cores: r.get("cpu_cores"),
        mem_total: r.get("mem_total"),
        swap_total: r.get("swap_total"),
        disk_total: r.get("disk_total"),
        agent_version: r.get("agent_version"),
        boot_at: r.get("boot_at"),
        capabilities: r.get("capabilities"),
        first_seen_at: r.get("first_seen_at"),
        last_seen_at: r.get("last_seen_at"),
    }
}

/// 对外的服务器标识。不用自增 id 是为了不暴露「你一共有几台机器」。
fn new_uuid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let mut buf = [0u8; 16];
    // 前 8 字节时间（保证单调，便于排查），后 8 字节随机
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    buf[..8].copy_from_slice(&t.to_be_bytes());
    if let Ok(r) = crate::auth::generate_token() {
        for (i, b) in r.bytes().take(8).enumerate() {
            buf[8 + i] = b;
        }
    }
    hex::encode(buf)
}

#[async_trait]
impl Storage for SqliteStore {
    // ── 管理员 ──

    async fn count_admins(&self) -> Result<i64> {
        Ok(sqlx::query("SELECT COUNT(*) FROM admin_user")
            .fetch_one(&self.read)
            .await?
            .get(0))
    }

    async fn create_admin(&self, username: &str, password_hash: &str, now: i64) -> Result<i64> {
        sqlx::query(
            "INSERT INTO admin_user (username, password_hash, created_at) VALUES (?, ?, ?)",
        )
        .bind(username)
        .bind(password_hash)
        .bind(now)
        .execute(&self.write)
        .await?;
        Ok(sqlx::query("SELECT id FROM admin_user WHERE username = ?")
            .bind(username)
            .fetch_one(&self.write)
            .await?
            .get(0))
    }

    async fn get_admin(&self, username: &str) -> Result<Option<AdminUser>> {
        Ok(
            sqlx::query("SELECT id, username, password_hash FROM admin_user WHERE username = ?")
                .bind(username)
                .fetch_optional(&self.read)
                .await?
                .map(|r| AdminUser {
                    id: r.get("id"),
                    username: r.get("username"),
                    password_hash: r.get("password_hash"),
                }),
        )
    }

    async fn touch_admin_login(&self, id: i64, now: i64) -> Result<()> {
        sqlx::query("UPDATE admin_user SET last_login_at = ? WHERE id = ?")
            .bind(now)
            .bind(id)
            .execute(&self.write)
            .await?;
        Ok(())
    }

    // ── refresh token 吊销 ──

    async fn revoke_token(&self, jti_hash: &str, expires_at: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO revoked_token (token_hash, expires_at) VALUES (?, ?)
             ON CONFLICT(token_hash) DO NOTHING",
        )
        .bind(jti_hash)
        .bind(expires_at)
        .execute(&self.write)
        .await?;
        Ok(())
    }

    async fn is_revoked(&self, jti_hash: &str) -> Result<bool> {
        Ok(
            sqlx::query("SELECT 1 FROM revoked_token WHERE token_hash = ?")
                .bind(jti_hash)
                .fetch_optional(&self.read)
                .await?
                .is_some(),
        )
    }

    async fn sweep_revoked(&self, now: i64) -> Result<u64> {
        Ok(
            sqlx::query("DELETE FROM revoked_token WHERE expires_at < ?")
                .bind(now)
                .execute(&self.write)
                .await?
                .rows_affected(),
        )
    }

    // ── 服务器管理 ──

    async fn create_server(&self, name: &str, token_hash: &str, now: i64) -> Result<ServerRecord> {
        let uuid = new_uuid();
        sqlx::query(
            "INSERT INTO servers (uuid, name, token_hash, first_seen_at,
                                  sort_order, hidden, location_manual)
             VALUES (?, ?, ?, ?,
                     COALESCE((SELECT MAX(sort_order) + 1 FROM servers), 0), 0, 0)",
        )
        .bind(&uuid)
        .bind(name)
        .bind(token_hash)
        .bind(now)
        .execute(&self.write)
        .await?;

        let row = sqlx::query(&format!("{SERVER_SELECT} WHERE uuid = ?"))
            .bind(&uuid)
            .fetch_one(&self.write)
            .await?;
        Ok(server_from_row(&row))
    }

    async fn list_servers(&self) -> Result<Vec<ServerRecord>> {
        Ok(
            sqlx::query(&format!("{SERVER_SELECT} ORDER BY sort_order, id"))
                .fetch_all(&self.read)
                .await?
                .iter()
                .map(server_from_row)
                .collect(),
        )
    }

    async fn get_server(&self, id: ServerId) -> Result<Option<ServerRecord>> {
        Ok(sqlx::query(&format!("{SERVER_SELECT} WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.read)
            .await?
            .as_ref()
            .map(server_from_row))
    }

    async fn get_server_by_token(&self, token_hash: &str) -> Result<Option<ServerRecord>> {
        Ok(
            sqlx::query(&format!("{SERVER_SELECT} WHERE token_hash = ?"))
                .bind(token_hash)
                .fetch_optional(&self.read)
                .await?
                .as_ref()
                .map(server_from_row),
        )
    }

    async fn get_server_by_uuid(&self, uuid: &str) -> Result<Option<ServerRecord>> {
        // uuid 上有 UNIQUE 索引，走的是索引查找
        Ok(sqlx::query(&format!("{SERVER_SELECT} WHERE uuid = ?"))
            .bind(uuid)
            .fetch_optional(&self.read)
            .await?
            .as_ref()
            .map(server_from_row))
    }

    async fn update_server(&self, id: ServerId, p: &ServerPatch) -> Result<bool> {
        // 逐字段构建 UPDATE：None 表示「不改这一项」，
        // Some(None) 表示「改成空值」—— 两者语义不同
        let mut qb = sqlx::QueryBuilder::new("UPDATE servers SET ");
        let mut sep = qb.separated(", ");
        let mut touched = false;
        macro_rules! set {
            ($field:ident, $col:literal) => {
                if let Some(v) = p.$field.clone() {
                    sep.push(concat!($col, " = ")).push_bind_unseparated(v);
                    touched = true;
                }
            };
        }
        set!(name, "name");
        set!(group_id, "group_id");
        set!(note, "note");
        set!(buy_url, "buy_url");
        set!(review_url, "review_url");
        set!(country_code, "country_code");
        set!(region, "region");
        set!(latitude, "latitude");
        set!(longitude, "longitude");
        if let Some(v) = p.hidden {
            sep.push("hidden = ").push_bind_unseparated(i64::from(v));
            touched = true;
        }
        if let Some(v) = p.location_manual {
            sep.push("location_manual = ")
                .push_bind_unseparated(i64::from(v));
            touched = true;
        }
        if !touched {
            return Ok(false);
        }
        qb.push(" WHERE id = ").push_bind(id);
        Ok(qb.build().execute(&self.write).await?.rows_affected() > 0)
    }

    async fn delete_server(&self, id: ServerId) -> Result<bool> {
        // 外键是 ON DELETE CASCADE，指标与延迟数据一并清掉
        Ok(sqlx::query("DELETE FROM servers WHERE id = ?")
            .bind(id)
            .execute(&self.write)
            .await?
            .rows_affected()
            > 0)
    }

    async fn set_server_token(&self, id: ServerId, token_hash: &str) -> Result<bool> {
        Ok(
            sqlx::query("UPDATE servers SET token_hash = ? WHERE id = ?")
                .bind(token_hash)
                .bind(id)
                .execute(&self.write)
                .await?
                .rows_affected()
                > 0,
        )
    }

    async fn reorder_servers(&self, order: &[(ServerId, i64)]) -> Result<()> {
        let mut tx = self.write.begin().await?;
        for (id, ord) in order {
            sqlx::query("UPDATE servers SET sort_order = ? WHERE id = ?")
                .bind(ord)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn update_server_facts(&self, id: ServerId, f: &ServerFacts, now: i64) -> Result<()> {
        // 名字只在用户还没改过（仍是默认占位）时才用 agent 上报的主机名覆盖 ——
        // 否则后台改的名字每次重连都会被冲掉
        sqlx::query(
            "UPDATE servers SET
                os = ?, kernel = ?, arch = ?, virtualization = ?,
                cpu_model = ?, cpu_cores = ?, mem_total = ?, swap_total = ?, disk_total = ?,
                agent_version = ?, boot_at = ?, capabilities = ?, last_ip = ?,
                last_seen_at = ?
             WHERE id = ?",
        )
        .bind(&f.os)
        .bind(&f.kernel)
        .bind(&f.arch)
        .bind(&f.virtualization)
        .bind(&f.cpu_model)
        .bind(f.cpu_cores)
        .bind(f.mem_total)
        .bind(f.swap_total)
        .bind(f.disk_total)
        .bind(&f.agent_version)
        .bind(f.boot_at)
        .bind(&f.capabilities)
        .bind(&f.last_ip)
        .bind(now)
        .bind(id)
        .execute(&self.write)
        .await?;
        Ok(())
    }

    // ── 运行期配置 ──

    async fn get_runtime_config(&self, id: ServerId) -> Result<pulse_proto::RuntimeConfig> {
        let Some(r) = sqlx::query(
            "SELECT interval_s, net_include, net_exclude, disk_include, disk_exclude,
                    gpu_enabled, report_temps, report_conn_count
             FROM server_runtime_config WHERE server_id = ?",
        )
        .bind(id)
        .fetch_optional(&self.read)
        .await?
        else {
            // 没有配置行时用协议默认值（含默认网卡黑名单）
            return Ok(pulse_proto::RuntimeConfig::default());
        };

        let list = |s: String| serde_json::from_str::<Vec<String>>(&s).unwrap_or_default();
        Ok(pulse_proto::RuntimeConfig {
            interval_s: r.get::<i64, _>("interval_s").clamp(1, 60) as u8,
            net_include: list(r.get("net_include")),
            net_exclude: list(r.get("net_exclude")),
            disk_include: list(r.get("disk_include")),
            disk_exclude: list(r.get("disk_exclude")),
            gpu_enabled: r.get::<i64, _>("gpu_enabled") != 0,
            report_temps: r.get::<i64, _>("report_temps") != 0,
            report_conn_count: r.get::<i64, _>("report_conn_count") != 0,
        }
        .sanitize())
    }

    async fn set_runtime_config(
        &self,
        id: ServerId,
        cfg: &pulse_proto::RuntimeConfig,
        now: i64,
    ) -> Result<()> {
        let j = |v: &Vec<String>| serde_json::to_string(v).unwrap_or_else(|_| "[]".into());
        sqlx::query(
            "INSERT INTO server_runtime_config
                (server_id, interval_s, net_include, net_exclude, disk_include, disk_exclude,
                 gpu_enabled, report_temps, report_conn_count, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(server_id) DO UPDATE SET
                interval_s = excluded.interval_s,
                net_include = excluded.net_include, net_exclude = excluded.net_exclude,
                disk_include = excluded.disk_include, disk_exclude = excluded.disk_exclude,
                gpu_enabled = excluded.gpu_enabled, report_temps = excluded.report_temps,
                report_conn_count = excluded.report_conn_count,
                updated_at = excluded.updated_at",
        )
        .bind(id)
        .bind(i64::from(cfg.interval_s.clamp(1, 60)))
        .bind(j(&cfg.net_include))
        .bind(j(&cfg.net_exclude))
        .bind(j(&cfg.disk_include))
        .bind(j(&cfg.disk_exclude))
        .bind(i64::from(cfg.gpu_enabled))
        .bind(i64::from(cfg.report_temps))
        .bind(i64::from(cfg.report_conn_count))
        .bind(now)
        .execute(&self.write)
        .await?;
        Ok(())
    }

    // ── 延迟监控任务 ──

    async fn create_ping_task(&self, t: &PingTaskInput, now: i64) -> Result<PingTaskRow> {
        let (sk, sids) = t.scope().to_columns();
        sqlx::query(
            "INSERT INTO ping_tasks
                (name, kind, host, port, expect_status, interval_s, packets, timeout_ms,
                 scope_kind, scope_ids, enabled, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&t.name)
        .bind(&t.kind)
        .bind(&t.host)
        .bind(t.port.map(i64::from))
        .bind(t.expect_status.map(i64::from))
        .bind(i64::from(t.interval_s))
        .bind(i64::from(t.packets))
        .bind(i64::from(t.timeout_ms))
        .bind(sk)
        .bind(&sids)
        .bind(i64::from(t.enabled))
        .bind(now)
        .execute(&self.write)
        .await?;

        let row = sqlx::query(&format!(
            "{PING_TASK_SELECT} WHERE id = (SELECT MAX(id) FROM ping_tasks)"
        ))
        .fetch_one(&self.write)
        .await?;
        Ok(ping_task_from_row(&row))
    }

    async fn list_ping_tasks(&self) -> Result<Vec<PingTaskRow>> {
        Ok(sqlx::query(&format!("{PING_TASK_SELECT} ORDER BY id"))
            .fetch_all(&self.read)
            .await?
            .iter()
            .map(ping_task_from_row)
            .collect())
    }

    async fn update_ping_task(&self, id: i64, t: &PingTaskInput) -> Result<bool> {
        let (sk, sids) = t.scope().to_columns();
        Ok(sqlx::query(
            "UPDATE ping_tasks SET
                name = ?, kind = ?, host = ?, port = ?, expect_status = ?,
                interval_s = ?, packets = ?, timeout_ms = ?,
                scope_kind = ?, scope_ids = ?, enabled = ?
             WHERE id = ?",
        )
        .bind(&t.name)
        .bind(&t.kind)
        .bind(&t.host)
        .bind(t.port.map(i64::from))
        .bind(t.expect_status.map(i64::from))
        .bind(i64::from(t.interval_s))
        .bind(i64::from(t.packets))
        .bind(i64::from(t.timeout_ms))
        .bind(sk)
        .bind(&sids)
        .bind(i64::from(t.enabled))
        .bind(id)
        .execute(&self.write)
        .await?
        .rows_affected()
            > 0)
    }

    async fn delete_ping_task(&self, id: i64) -> Result<bool> {
        // 结果行不级联删除：任务没了，但已经采到的历史数据还有价值。
        // 它们会按正常的保留期自然过期。
        Ok(sqlx::query("DELETE FROM ping_tasks WHERE id = ?")
            .bind(id)
            .execute(&self.write)
            .await?
            .rows_affected()
            > 0)
    }

    async fn ping_tasks_for_server(&self, id: ServerId) -> Result<Vec<pulse_proto::PingTaskSpec>> {
        // 作用范围在 Rust 里判断而不是写进 SQL：scope_ids 是 JSON，
        // 用 SQL 的 JSON 函数会把 SQLite 与 PostgreSQL 的方言差异带进来
        let group_id: Option<i64> = sqlx::query("SELECT group_id FROM servers WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.read)
            .await?
            .and_then(|r| r.get("group_id"));

        Ok(
            sqlx::query(&format!("{PING_TASK_SELECT} WHERE enabled = 1 ORDER BY id"))
                .fetch_all(&self.read)
                .await?
                .iter()
                .map(ping_task_from_row)
                .filter(|t| t.scope.covers(id, group_id))
                .map(|t| t.to_spec())
                .collect(),
        )
    }

    // ── 账单 ──

    async fn get_billing(&self, id: ServerId) -> Result<Option<Billing>> {
        Ok(sqlx::query(
            "SELECT price, currency, cycle, custom_cycle_days, cycle_start_at,
                    expire_at, auto_renew, purchased_at, remark
             FROM server_billing WHERE server_id = ?",
        )
        .bind(id)
        .fetch_optional(&self.read)
        .await?
        .map(|r| billing_from_row(&r)))
    }

    async fn set_billing(&self, id: ServerId, b: &BillingInput, now: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO server_billing
                (server_id, price, currency, cycle, custom_cycle_days, cycle_start_at,
                 expire_at, auto_renew, purchased_at, remark, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(server_id) DO UPDATE SET
                price = excluded.price, currency = excluded.currency, cycle = excluded.cycle,
                custom_cycle_days = excluded.custom_cycle_days,
                cycle_start_at = excluded.cycle_start_at, expire_at = excluded.expire_at,
                auto_renew = excluded.auto_renew, purchased_at = excluded.purchased_at,
                remark = excluded.remark, updated_at = excluded.updated_at",
        )
        .bind(id)
        .bind(b.price)
        .bind(b.currency.to_ascii_uppercase())
        .bind(&b.cycle)
        .bind(b.custom_cycle_days)
        .bind(b.cycle_start_at)
        .bind(b.expire_at)
        .bind(i64::from(b.auto_renew))
        .bind(b.purchased_at)
        .bind(&b.remark)
        .bind(now)
        .execute(&self.write)
        .await?;
        Ok(())
    }

    async fn delete_billing(&self, id: ServerId) -> Result<bool> {
        Ok(
            sqlx::query("DELETE FROM server_billing WHERE server_id = ?")
                .bind(id)
                .execute(&self.write)
                .await?
                .rows_affected()
                > 0,
        )
    }

    async fn all_billing(&self) -> Result<Vec<BillingEntry>> {
        // LEFT JOIN：没填账单的机器也要出现在结果里，
        // 这样汇总才能如实报出「N 台未填价格」
        Ok(sqlx::query(
            "SELECT s.id AS sid, s.uuid, s.name, s.country_code, s.latitude, s.longitude,
                    s.group_id, s.buy_url, s.review_url,
                    s.hidden, s.os, s.arch, s.cpu_cores, s.mem_total,
                    s.disk_total, s.agent_version, s.last_seen_at,
                    b.price, b.currency, b.cycle, b.custom_cycle_days,
                    b.cycle_start_at, b.expire_at, b.auto_renew,
                    b.purchased_at, b.remark
             FROM servers s LEFT JOIN server_billing b ON b.server_id = s.id
             ORDER BY s.sort_order, s.id",
        )
        .fetch_all(&self.read)
        .await?
        .iter()
        .map(|r| {
            let price: Option<f64> = r.get("price");
            BillingEntry {
                id: r.get("sid"),
                uuid: r.get("uuid"),
                name: r.get("name"),
                country_code: r.get("country_code"),
                buy_url: r.get("buy_url"),
                review_url: r.get("review_url"),
                latitude: r.get("latitude"),
                longitude: r.get("longitude"),
                group_id: r.get("group_id"),
                hidden: r.get::<i64, _>("hidden") != 0,
                os: r.get("os"),
                arch: r.get("arch"),
                cpu_cores: r.get("cpu_cores"),
                mem_total: r.get("mem_total"),
                disk_total: r.get("disk_total"),
                agent_version: r.get("agent_version"),
                last_seen_at: r.get("last_seen_at"),
                billing: price.map(|_| billing_from_row(r)),
            }
        })
        .collect())
    }

    // ── 流量 ──

    async fn get_traffic(&self, id: ServerId) -> Result<Option<TrafficRow>> {
        Ok(sqlx::query(
            "SELECT server_id, period_start, in_bytes, out_bytes,
                    last_raw_in, last_raw_out, alerted_pct
             FROM server_traffic WHERE server_id = ?",
        )
        .bind(id)
        .fetch_optional(&self.read)
        .await?
        .map(|r| TrafficRow {
            server_id: r.get("server_id"),
            period_start: r.get("period_start"),
            in_bytes: r.get("in_bytes"),
            out_bytes: r.get("out_bytes"),
            last_raw_in: r.get("last_raw_in"),
            last_raw_out: r.get("last_raw_out"),
            alerted_pct: serde_json::from_str(&r.get::<String, _>("alerted_pct"))
                .unwrap_or_default(),
        }))
    }

    async fn upsert_traffic(&self, t: &TrafficRow, now: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO server_traffic
                (server_id, period_start, in_bytes, out_bytes,
                 last_raw_in, last_raw_out, last_raw_at, alerted_pct, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(server_id) DO UPDATE SET
                period_start = excluded.period_start,
                in_bytes = excluded.in_bytes, out_bytes = excluded.out_bytes,
                last_raw_in = excluded.last_raw_in, last_raw_out = excluded.last_raw_out,
                last_raw_at = excluded.last_raw_at,
                alerted_pct = excluded.alerted_pct, updated_at = excluded.updated_at",
        )
        .bind(t.server_id)
        .bind(t.period_start)
        .bind(t.in_bytes)
        .bind(t.out_bytes)
        .bind(t.last_raw_in)
        .bind(t.last_raw_out)
        .bind(now)
        .bind(serde_json::to_string(&t.alerted_pct).unwrap_or_else(|_| "[]".into()))
        .bind(now)
        .execute(&self.write)
        .await?;
        Ok(())
    }

    async fn archive_traffic(
        &self,
        id: ServerId,
        period_start: i64,
        period_end: i64,
        in_bytes: i64,
        out_bytes: i64,
    ) -> Result<()> {
        // 幂等：主键是 (server_id, period_start)，补做时重复执行是 upsert
        sqlx::query(
            "INSERT INTO traffic_history (server_id, period_start, period_end, in_bytes, out_bytes)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(server_id, period_start) DO UPDATE SET
                period_end = excluded.period_end,
                in_bytes = excluded.in_bytes, out_bytes = excluded.out_bytes",
        )
        .bind(id)
        .bind(period_start)
        .bind(period_end)
        .bind(in_bytes)
        .bind(out_bytes)
        .execute(&self.write)
        .await?;
        Ok(())
    }

    async fn get_traffic_config(&self, id: ServerId) -> Result<TrafficConfig> {
        Ok(sqlx::query(
            "SELECT limit_bytes, calc_mode, reset_day, timezone, alert_pct
             FROM server_traffic_config WHERE server_id = ?",
        )
        .bind(id)
        .fetch_optional(&self.read)
        .await?
        .map(|r| TrafficConfig {
            limit_bytes: r.get("limit_bytes"),
            calc_mode: r.get("calc_mode"),
            reset_day: (r.get::<i64, _>("reset_day").clamp(1, 31)) as u32,
            timezone: r.get("timezone"),
            alert_pct: serde_json::from_str(&r.get::<String, _>("alert_pct"))
                .unwrap_or_else(|_| vec![80, 95, 100]),
        })
        .unwrap_or_default())
    }

    async fn set_traffic_config(&self, id: ServerId, c: &TrafficConfig) -> Result<()> {
        sqlx::query(
            "INSERT INTO server_traffic_config
                (server_id, limit_bytes, calc_mode, reset_day, timezone, alert_pct)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(server_id) DO UPDATE SET
                limit_bytes = excluded.limit_bytes, calc_mode = excluded.calc_mode,
                reset_day = excluded.reset_day, timezone = excluded.timezone,
                alert_pct = excluded.alert_pct",
        )
        .bind(id)
        .bind(c.limit_bytes)
        .bind(&c.calc_mode)
        .bind(i64::from(c.reset_day.clamp(1, 31)))
        .bind(&c.timezone)
        .bind(serde_json::to_string(&c.alert_pct).unwrap_or_else(|_| "[80,95,100]".into()))
        .execute(&self.write)
        .await?;
        Ok(())
    }

    // ── 套餐与分组 ──

    async fn list_plans(&self) -> Result<Vec<PlanRow>> {
        Ok(sqlx::query(&format!("{PLAN_SELECT} ORDER BY id"))
            .fetch_all(&self.read)
            .await?
            .iter()
            .map(plan_from_row)
            .collect())
    }

    async fn create_plan(&self, p: &PlanRow, now: i64) -> Result<PlanRow> {
        sqlx::query(
            "INSERT INTO plans (name, provider, buy_url, review_url, price, currency, cycle, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&p.name).bind(&p.provider).bind(&p.buy_url).bind(&p.review_url)
        .bind(p.price).bind(&p.currency).bind(&p.cycle).bind(now)
        .execute(&self.write).await?;
        let row = sqlx::query(&format!(
            "{PLAN_SELECT} WHERE id = (SELECT MAX(id) FROM plans)"
        ))
        .fetch_one(&self.write)
        .await?;
        Ok(plan_from_row(&row))
    }

    async fn update_plan(&self, id: i64, p: &PlanRow) -> Result<bool> {
        Ok(sqlx::query(
            "UPDATE plans SET name=?, provider=?, buy_url=?, review_url=?,
                              price=?, currency=?, cycle=? WHERE id=?",
        )
        .bind(&p.name)
        .bind(&p.provider)
        .bind(&p.buy_url)
        .bind(&p.review_url)
        .bind(p.price)
        .bind(&p.currency)
        .bind(&p.cycle)
        .bind(id)
        .execute(&self.write)
        .await?
        .rows_affected()
            > 0)
    }

    async fn delete_plan(&self, id: i64) -> Result<bool> {
        // 外键是 ON DELETE SET NULL：删套餐不会删机器
        Ok(sqlx::query("DELETE FROM plans WHERE id = ?")
            .bind(id)
            .execute(&self.write)
            .await?
            .rows_affected()
            > 0)
    }

    async fn list_groups(&self) -> Result<Vec<GroupRow>> {
        Ok(sqlx::query(
            "SELECT id, name, color, icon, sort_order FROM groups ORDER BY sort_order, id",
        )
        .fetch_all(&self.read)
        .await?
        .iter()
        .map(group_from_row)
        .collect())
    }

    async fn create_group(&self, g: &GroupRow, now: i64) -> Result<GroupRow> {
        sqlx::query(
            "INSERT INTO groups (name, color, icon, sort_order, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&g.name)
        .bind(&g.color)
        .bind(&g.icon)
        .bind(g.sort_order)
        .bind(now)
        .execute(&self.write)
        .await?;
        let row =
            sqlx::query("SELECT id, name, color, icon, sort_order FROM groups WHERE name = ?")
                .bind(&g.name)
                .fetch_one(&self.write)
                .await?;
        Ok(group_from_row(&row))
    }

    async fn update_group(&self, id: i64, g: &GroupRow) -> Result<bool> {
        Ok(
            sqlx::query("UPDATE groups SET name=?, color=?, icon=?, sort_order=? WHERE id=?")
                .bind(&g.name)
                .bind(&g.color)
                .bind(&g.icon)
                .bind(g.sort_order)
                .bind(id)
                .execute(&self.write)
                .await?
                .rows_affected()
                > 0,
        )
    }

    async fn delete_group(&self, id: i64) -> Result<bool> {
        // 外键 ON DELETE SET NULL：机器会变成「未分组」而不是被删掉
        Ok(sqlx::query("DELETE FROM groups WHERE id = ?")
            .bind(id)
            .execute(&self.write)
            .await?
            .rows_affected()
            > 0)
    }

    // ── 汇率 ──

    async fn load_rates(&self) -> Result<Rates> {
        let rows = sqlx::query("SELECT quote, rate, source, as_of, fetched_at FROM exchange_rates")
            .fetch_all(&self.read)
            .await?;
        // fetched_at 取所有自动拉取记录里最新的那个 —— 它决定「是否陈旧」
        let fetched_at = rows
            .iter()
            .filter(|r| r.get::<String, _>("source") == "frankfurter")
            .map(|r| r.get::<i64, _>("fetched_at"))
            .max()
            .unwrap_or(0);
        let as_of = rows
            .iter()
            .filter(|r| r.get::<String, _>("source") == "frankfurter")
            .max_by_key(|r| r.get::<i64, _>("fetched_at"))
            .map(|r| r.get::<String, _>("as_of"))
            .unwrap_or_default();

        let mut out = Rates::new(fetched_at, as_of);
        // 先插自动的再插人工的？不行 —— insert 会挡住覆盖人工值。
        // 反过来先插人工的，自动的就会被正确挡住。
        for r in rows
            .iter()
            .filter(|r| r.get::<String, _>("source") == "manual")
        {
            out.insert(&r.get::<String, _>("quote"), r.get("rate"), Source::Manual);
        }
        for r in rows
            .iter()
            .filter(|r| r.get::<String, _>("source") != "manual")
        {
            out.insert(
                &r.get::<String, _>("quote"),
                r.get("rate"),
                Source::Frankfurter,
            );
        }
        Ok(out)
    }

    async fn save_rates(&self, r: &Rates, now: i64) -> Result<()> {
        let mut tx = self.write.begin().await?;
        for q in r.currencies() {
            let Some(e) = r.get(&q) else { continue };
            if e.source == Source::Manual {
                continue; // 人工值由 set_manual_rate 单独管理，不在这里覆写
            }
            sqlx::query(
                "INSERT INTO exchange_rates (quote, rate, source, as_of, fetched_at)
                 VALUES (?, ?, 'frankfurter', ?, ?)
                 ON CONFLICT(quote) DO UPDATE SET
                    rate = excluded.rate, as_of = excluded.as_of, fetched_at = excluded.fetched_at
                 WHERE exchange_rates.source != 'manual'",
            )
            .bind(&q)
            .bind(e.rate)
            .bind(&r.as_of)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn set_manual_rate(&self, quote: &str, rate: f64, now: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO exchange_rates (quote, rate, source, as_of, fetched_at)
             VALUES (?, ?, 'manual', 'manual', ?)
             ON CONFLICT(quote) DO UPDATE SET
                rate = excluded.rate, source = 'manual', as_of = 'manual',
                fetched_at = excluded.fetched_at",
        )
        .bind(quote.trim().to_ascii_uppercase())
        .bind(rate)
        .bind(now)
        .execute(&self.write)
        .await?;
        Ok(())
    }

    async fn delete_manual_rate(&self, quote: &str) -> Result<bool> {
        Ok(
            sqlx::query("DELETE FROM exchange_rates WHERE quote = ? AND source = 'manual'")
                .bind(quote.trim().to_ascii_uppercase())
                .execute(&self.write)
                .await?
                .rows_affected()
                > 0,
        )
    }

    // ── 通知渠道 ──

    async fn list_channels(&self, secret: &[u8]) -> Result<Vec<ChannelRow>> {
        let rows = sqlx::query(
            "SELECT id, name, kind, config_enc, enabled FROM notification_channels ORDER BY id",
        )
        .fetch_all(&self.read)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let id: i64 = r.get("id");
            let blob: Vec<u8> = r.get("config_enc");
            // 解不开的渠道跳过并记日志，而不是让整个列表失败 ——
            // 换过密钥的话所有渠道都解不开，那时至少还能看到其他配置
            let json = match crypto::decrypt(secret, &blob) {
                Ok(j) => j,
                Err(e) => {
                    warn!(id, "渠道凭据解密失败（密钥变了？）: {e}");
                    continue;
                }
            };
            let config = match serde_json::from_str(&json) {
                Ok(c) => c,
                Err(e) => {
                    warn!(id, "渠道配置解析失败: {e}");
                    continue;
                }
            };
            out.push(ChannelRow {
                id,
                name: r.get("name"),
                kind: r.get("kind"),
                config,
                enabled: r.get::<i64, _>("enabled") != 0,
            });
        }
        Ok(out)
    }

    async fn create_channel(&self, c: &ChannelInput, secret: &[u8], now: i64) -> Result<i64> {
        let enc = encrypt_config(&c.config, secret)?;
        sqlx::query(
            "INSERT INTO notification_channels (name, kind, config_enc, enabled, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&c.name)
        .bind(c.config.kind().as_str())
        .bind(enc)
        .bind(i64::from(c.enabled))
        .bind(now)
        .execute(&self.write)
        .await?;
        Ok(sqlx::query("SELECT MAX(id) FROM notification_channels")
            .fetch_one(&self.write)
            .await?
            .get(0))
    }

    async fn update_channel(&self, id: i64, c: &ChannelInput, secret: &[u8]) -> Result<bool> {
        let enc = encrypt_config(&c.config, secret)?;
        Ok(sqlx::query(
            "UPDATE notification_channels SET name = ?, kind = ?, config_enc = ?, enabled = ?
             WHERE id = ?",
        )
        .bind(&c.name)
        .bind(c.config.kind().as_str())
        .bind(enc)
        .bind(i64::from(c.enabled))
        .bind(id)
        .execute(&self.write)
        .await?
        .rows_affected()
            > 0)
    }

    async fn delete_channel(&self, id: i64) -> Result<bool> {
        Ok(
            sqlx::query("DELETE FROM notification_channels WHERE id = ?")
                .bind(id)
                .execute(&self.write)
                .await?
                .rows_affected()
                > 0,
        )
    }

    // ── 通知规则 ──

    async fn list_rules(&self) -> Result<Vec<RuleRow>> {
        Ok(sqlx::query(&format!("{RULE_SELECT} ORDER BY id"))
            .fetch_all(&self.read)
            .await?
            .iter()
            .map(rule_from_row)
            .collect())
    }

    async fn create_rule(&self, r: &RuleRow, now: i64) -> Result<i64> {
        let (sk, sids) = r.scope().to_columns();
        sqlx::query(
            "INSERT INTO notification_rules
                (name, event_kinds, channel_ids, scope_kind, scope_ids, params,
                 duration_s, cooldown_s, notify_resolve, title_tpl, body_tpl, enabled, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&r.name)
        .bind(json_or_empty(&r.event_kinds))
        .bind(json_or_empty(&r.channel_ids))
        .bind(sk)
        .bind(&sids)
        .bind(r.params.to_string())
        .bind(r.duration_s)
        .bind(r.cooldown_s)
        .bind(i64::from(r.notify_resolve))
        .bind(&r.title_tpl)
        .bind(&r.body_tpl)
        .bind(i64::from(r.enabled))
        .bind(now)
        .execute(&self.write)
        .await?;
        Ok(sqlx::query("SELECT MAX(id) FROM notification_rules")
            .fetch_one(&self.write)
            .await?
            .get(0))
    }

    async fn update_rule(&self, id: i64, r: &RuleRow) -> Result<bool> {
        let (sk, sids) = r.scope().to_columns();
        Ok(sqlx::query(
            "UPDATE notification_rules SET
                name=?, event_kinds=?, channel_ids=?, scope_kind=?, scope_ids=?, params=?,
                duration_s=?, cooldown_s=?, notify_resolve=?, title_tpl=?, body_tpl=?, enabled=?
             WHERE id=?",
        )
        .bind(&r.name)
        .bind(json_or_empty(&r.event_kinds))
        .bind(json_or_empty(&r.channel_ids))
        .bind(sk)
        .bind(&sids)
        .bind(r.params.to_string())
        .bind(r.duration_s)
        .bind(r.cooldown_s)
        .bind(i64::from(r.notify_resolve))
        .bind(&r.title_tpl)
        .bind(&r.body_tpl)
        .bind(i64::from(r.enabled))
        .bind(id)
        .execute(&self.write)
        .await?
        .rows_affected()
            > 0)
    }

    async fn delete_rule(&self, id: i64) -> Result<bool> {
        // 外键 CASCADE：规则删掉时它的告警记录一并清理
        Ok(sqlx::query("DELETE FROM notification_rules WHERE id = ?")
            .bind(id)
            .execute(&self.write)
            .await?
            .rows_affected()
            > 0)
    }

    // ── 告警状态机 ──

    async fn get_alert(
        &self,
        rule_id: i64,
        server_id: Option<ServerId>,
        kind: &str,
    ) -> Result<Option<AlertEvent>> {
        Ok(sqlx::query(
            "SELECT state, first_at, fired_at, resolved_at, last_notified_at
             FROM notification_events
             WHERE rule_id = ? AND server_id IS ? AND event_kind = ? AND state != 'resolved'",
        )
        .bind(rule_id)
        .bind(server_id)
        .bind(kind)
        .fetch_optional(&self.read)
        .await?
        .and_then(|r| {
            Some(AlertEvent {
                state: AlertState::parse(&r.get::<String, _>("state"))?,
                first_at: r.get("first_at"),
                fired_at: r.get("fired_at"),
                resolved_at: r.get("resolved_at"),
                last_notified_at: r.get("last_notified_at"),
            })
        }))
    }

    async fn upsert_alert(
        &self,
        rule_id: i64,
        server_id: Option<ServerId>,
        kind: &str,
        e: &AlertEvent,
        payload: Option<&str>,
    ) -> Result<()> {
        let mut tx = self.write.begin().await?;
        // 先删活跃记录再插：唯一索引是部分索引（WHERE state != 'resolved'），
        // ON CONFLICT 在部分索引上不好写，直接删插更清楚
        sqlx::query(
            "DELETE FROM notification_events
             WHERE rule_id = ? AND server_id IS ? AND event_kind = ? AND state != 'resolved'",
        )
        .bind(rule_id)
        .bind(server_id)
        .bind(kind)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO notification_events
                (rule_id, server_id, event_kind, state, first_at, fired_at,
                 resolved_at, last_notified_at, payload)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(rule_id)
        .bind(server_id)
        .bind(kind)
        .bind(e.state.as_str())
        .bind(e.first_at)
        .bind(e.fired_at)
        .bind(e.resolved_at)
        .bind(e.last_notified_at)
        .bind(payload)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn clear_alert(
        &self,
        rule_id: i64,
        server_id: Option<ServerId>,
        kind: &str,
    ) -> Result<()> {
        sqlx::query(
            "DELETE FROM notification_events
             WHERE rule_id = ? AND server_id IS ? AND event_kind = ? AND state != 'resolved'",
        )
        .bind(rule_id)
        .bind(server_id)
        .bind(kind)
        .execute(&self.write)
        .await?;
        Ok(())
    }

    async fn list_alerts(&self, limit: i64) -> Result<Vec<AlertRow>> {
        Ok(sqlx::query(
            "SELECT id, rule_id, server_id, event_kind, state, first_at, fired_at,
                    resolved_at, last_notified_at, payload
             FROM notification_events ORDER BY first_at DESC LIMIT ?",
        )
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.read)
        .await?
        .iter()
        .map(|r| AlertRow {
            id: r.get("id"),
            rule_id: r.get("rule_id"),
            server_id: r.get("server_id"),
            event_kind: r.get("event_kind"),
            state: r.get("state"),
            first_at: r.get("first_at"),
            fired_at: r.get("fired_at"),
            resolved_at: r.get("resolved_at"),
            last_notified_at: r.get("last_notified_at"),
            payload: r.get("payload"),
        })
        .collect())
    }

    // ── 站点设置 ──

    async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(sqlx::query("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.read)
            .await?
            .map(|r| r.get("value")))
    }

    async fn set_setting(&self, key: &str, value: &str, now: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(key)
        .bind(value)
        .bind(now)
        .execute(&self.write)
        .await?;
        Ok(())
    }

    async fn insert_metrics(&self, layer: MetricLayer, rows: &[MetricRow]) -> Result<u64> {
        if rows.is_empty() {
            return Ok(0);
        }
        let table = layer.table();
        let cols = metric_col_list();
        let mut total = 0u64;
        // 整批一个事务。每行一个事务是 SQLite 慢的根源。
        let mut tx = self.write.begin().await?;
        for chunk in rows.chunks(METRIC_ROWS_PER_STMT) {
            let mut qb =
                sqlx::QueryBuilder::new(format!("INSERT INTO {table} (server_id, ts, {cols}) "));
            qb.push_values(chunk, |mut b, r| {
                b.push_bind(r.server_id).push_bind(r.ts);
                for v in r.values() {
                    b.push_bind(v);
                }
            });
            // 同一 (server_id, ts) 重复上报时覆盖而不是报错 —— 补报场景必须幂等
            qb.push(" ON CONFLICT(server_id, ts) DO UPDATE SET ");
            qb.push(
                METRIC_COLS
                    .iter()
                    .map(|(c, _)| format!("{c} = excluded.{c}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            total += qb.build().execute(&mut *tx).await?.rows_affected();
        }
        tx.commit().await?;
        Ok(total)
    }

    async fn insert_ping(&self, layer: PingLayer, rows: &[PingRow]) -> Result<u64> {
        if rows.is_empty() {
            return Ok(0);
        }
        let table = layer.table();
        let mut total = 0u64;
        let mut tx = self.write.begin().await?;
        for chunk in rows.chunks(PING_ROWS_PER_STMT) {
            let mut qb = sqlx::QueryBuilder::new(format!(
                "INSERT INTO {table} (server_id, task_id, ts, rtt_avg, rtt_min, rtt_max, sent, recv) "
            ));
            qb.push_values(chunk, |mut b, r| {
                b.push_bind(r.server_id)
                    .push_bind(r.task_id)
                    .push_bind(r.ts)
                    .push_bind(r.rtt_avg)
                    .push_bind(r.rtt_min)
                    .push_bind(r.rtt_max)
                    .push_bind(r.sent)
                    .push_bind(r.recv);
            });
            qb.push(
                " ON CONFLICT(server_id, task_id, ts) DO UPDATE SET
                  rtt_avg = excluded.rtt_avg, rtt_min = excluded.rtt_min,
                  rtt_max = excluded.rtt_max, sent = excluded.sent, recv = excluded.recv",
            );
            total += qb.build().execute(&mut *tx).await?.rows_affected();
        }
        tx.commit().await?;
        Ok(total)
    }

    async fn query_metrics(&self, server: ServerId, from: i64, to: i64) -> Result<MetricSeries> {
        let plan = plan_query((to - from).max(0));
        let table = match plan.granularity {
            Granularity::Minute => "metrics_minute",
            Granularity::Hour => "metrics_hour",
        };
        // 只取图表实际要画的列。全 22 列会让响应体和反序列化成本白白翻几倍。
        let sql = format!(
            "SELECT (ts / ?) * ? AS b,
                    CAST(AVG(cpu_pct) AS INTEGER)       AS cpu_pct,
                    CAST(AVG(mem_total) AS INTEGER)     AS mem_total,
                    CAST(AVG(mem_available) AS INTEGER) AS mem_available,
                    CAST(AVG(disk_used) AS INTEGER)     AS disk_used,
                    CAST(AVG(disk_total) AS INTEGER)    AS disk_total,
                    CAST(AVG(net_in_speed) AS INTEGER)  AS net_in_speed,
                    CAST(AVG(net_out_speed) AS INTEGER) AS net_out_speed,
                    CAST(AVG(load1) AS INTEGER)         AS load1,
                    CAST(AVG(swap_used) AS INTEGER)     AS swap_used,
                    CAST(AVG(tcp_conn) AS INTEGER)      AS tcp_conn,
                    CAST(AVG(udp_conn) AS INTEGER)      AS udp_conn,
                    CAST(AVG(proc_count) AS INTEGER)    AS proc_count
             FROM {table}
             WHERE server_id = ? AND ts >= ? AND ts < ?
             GROUP BY b ORDER BY b"
        );
        let rows = sqlx::query(&sql)
            .bind(plan.step)
            .bind(plan.step)
            .bind(server)
            .bind(from)
            .bind(to)
            .fetch_all(&self.read)
            .await?;

        let mut s = MetricSeries {
            granularity: Some(plan.granularity),
            step: plan.step,
            ..Default::default()
        };
        for r in &rows {
            s.ts.push(r.get::<i64, _>("b"));
            s.cpu_pct.push(r.get("cpu_pct"));
            s.mem_total.push(r.get("mem_total"));
            s.mem_available.push(r.get("mem_available"));
            s.disk_used.push(r.get("disk_used"));
            s.disk_total.push(r.get("disk_total"));
            s.net_in_speed.push(r.get("net_in_speed"));
            s.net_out_speed.push(r.get("net_out_speed"));
            s.load1.push(r.get("load1"));
            s.swap_used.push(r.get("swap_used"));
            s.tcp_conn.push(r.get("tcp_conn"));
            s.udp_conn.push(r.get("udp_conn"));
            s.proc_count.push(r.get("proc_count"));
        }
        debug!(points = s.len(), step = plan.step, table, "query_metrics");
        Ok(s)
    }

    async fn query_ping(
        &self,
        server: ServerId,
        task: i64,
        from: i64,
        to: i64,
    ) -> Result<PingSeries> {
        let range = (to - from).max(0);
        // 延迟是三层，与指标的两层不同：24h 内走 raw，7d 内走 5m，更长走 hour
        let (table, base) = if range <= 6 * 3600 {
            ("ping_raw", 60)
        } else if range <= 7 * 86_400 {
            ("ping_5m", 300)
        } else {
            ("ping_hour", 3600)
        };
        let raw = (range / base).max(1);
        let factor = ((raw + super::MAX_POINTS - 1) / super::MAX_POINTS).max(1);
        let step = base * factor;

        let sql = format!(
            "SELECT (ts / ?) * ? AS b,
                    CAST(SUM(rtt_avg * recv) / NULLIF(SUM(recv), 0) AS INTEGER) AS rtt_avg,
                    MIN(rtt_min) AS rtt_min, MAX(rtt_max) AS rtt_max,
                    SUM(sent) AS sent, SUM(recv) AS recv
             FROM {table}
             WHERE server_id = ? AND task_id = ? AND ts >= ? AND ts < ?
             GROUP BY b ORDER BY b"
        );
        let rows = sqlx::query(&sql)
            .bind(step)
            .bind(step)
            .bind(server)
            .bind(task)
            .bind(from)
            .bind(to)
            .fetch_all(&self.read)
            .await?;

        let mut s = PingSeries {
            granularity: None,
            step,
            ..Default::default()
        };
        for r in &rows {
            s.ts.push(r.get::<i64, _>("b"));
            s.rtt_avg.push(r.get("rtt_avg"));
            s.rtt_min.push(r.get("rtt_min"));
            s.rtt_max.push(r.get("rtt_max"));
            let sent: i64 = r.get("sent");
            let recv: i64 = r.get("recv");
            // 丢包率由计数现算。用 f64 真百分比，和实时那条路一致
            s.loss_pct
                .push((sent > 0).then(|| (sent - recv) as f64 * 100.0 / sent as f64));
        }
        Ok(s)
    }

    async fn rollup(&self, now: i64) -> Result<RollupReport> {
        let mut rep = RollupReport {
            caught_up: true,
            ..Default::default()
        };

        // 只上卷**已经结束**的时间窗，进行中的窗口留到下一轮。
        //
        // 注：agent 断线补报最多 5 分钟，永远落在
        // 当前这个未完成的小时内，所以补报数据不会被水位线漏掉。
        let hour_end = (now / 3600) * 3600;
        let min5_end = (now / 300) * 300;

        // 每层：起点 = 水位线（或源表最早数据），终点 = min(对齐终点, 起点 + 窗口上限)
        let mut step = |from: i64, end: i64, bucket: i64| -> Option<(i64, i64)> {
            if from >= end {
                return None;
            }
            let capped = from + bucket * ROLLUP_MAX_BUCKETS;
            let to = capped.min(end);
            if to < end {
                rep.caught_up = false; // 这一层还有积压
            }
            Some((from, to))
        };

        // 指标：分钟 → 小时
        if let Some(from) = self.rollup_from("metrics_hour", "metrics_minute").await? {
            if let Some((f, t)) = step(from, hour_end, 3600) {
                rep.metrics_hour = self.rollup_metrics(f, t).await?;
                self.set_watermark("metrics_hour", t).await?;
            }
        }

        // 延迟：raw → 5m
        if let Some(from) = self.rollup_from("ping_5m", "ping_raw").await? {
            if let Some((f, t)) = step(from, min5_end, 300) {
                rep.ping_5m = self.rollup_ping("ping_raw", "ping_5m", 300, f, t).await?;
                self.set_watermark("ping_5m", t).await?;
            }
        }

        // 延迟：5m → hour。从 5m 上卷而不是从 raw —— raw 只留 24 小时，
        // 而 hour 层要覆盖 30 天，直接从 raw 卷会在断线超过一天后出现空洞。
        if let Some(from) = self.rollup_from("ping_hour", "ping_5m").await? {
            if let Some((f, t)) = step(from, hour_end, 3600) {
                rep.ping_hour = self.rollup_ping("ping_5m", "ping_hour", 3600, f, t).await?;
                self.set_watermark("ping_hour", t).await?;
            }
        }

        Ok(rep)
    }

    async fn prune(&self, p: &RetentionPolicy, now: i64) -> Result<PruneReport> {
        let mut rep = PruneReport::default();
        let day = 86_400;

        for (table, cutoff, slot) in [
            ("metrics_minute", now - p.minute_days * day, 0usize),
            ("metrics_hour", now - p.hour_days * day, 1),
            ("ping_raw", now - p.ping_raw_hours * 3600, 2),
            ("ping_5m", now - p.ping_5m_days * day, 3),
            ("ping_hour", now - p.ping_hour_days * day, 4),
        ] {
            let (n, chunks, ms) = self.prune_table(table, cutoff).await?;
            match slot {
                0 => rep.metrics_minute = n,
                1 => rep.metrics_hour = n,
                2 => rep.ping_raw = n,
                3 => rep.ping_5m = n,
                _ => rep.ping_hour = n,
            }
            rep.chunks += chunks;
            rep.max_chunk_ms = rep.max_chunk_ms.max(ms);
        }
        Ok(rep)
    }

    async fn stats(&self) -> Result<StorageStats> {
        let pragma = |k: &'static str| async move {
            sqlx::query(&format!("PRAGMA {k}"))
                .fetch_one(&self.read)
                .await
                .map(|r| r.get::<i64, _>(0))
        };
        let page_size = pragma("page_size").await?;
        let page_count = pragma("page_count").await?;
        let freelist_count = pragma("freelist_count").await?;

        let mut rows = BTreeMap::new();
        for t in [
            "servers",
            "metrics_minute",
            "metrics_hour",
            "ping_raw",
            "ping_5m",
            "ping_hour",
            "traffic_history",
        ] {
            let n: i64 = sqlx::query(&format!("SELECT COUNT(*) FROM {t}"))
                .fetch_one(&self.read)
                .await?
                .get(0);
            rows.insert(t.to_string(), n);
        }

        Ok(StorageStats {
            driver: "sqlite",
            size_bytes: page_size * page_count,
            page_size,
            page_count,
            freelist_count,
            rows,
        })
    }
}
