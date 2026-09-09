-- M1 存储层初始 schema。方言：SQLite。
-- PostgreSQL 版本放在 migrations/postgres/，两套分开维护（见 docs/02-data-model.md）。

-- ---------------------------------------------------------------------------
-- servers：M1 只建指标表所需的最小集，M3 扩展为完整的服务器管理表
-- ---------------------------------------------------------------------------
CREATE TABLE servers (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    uuid          TEXT    NOT NULL UNIQUE,
    name          TEXT    NOT NULL,
    first_seen_at INTEGER NOT NULL,
    last_seen_at  INTEGER
);

-- ---------------------------------------------------------------------------
-- 指标：分钟层（短保留）+ 小时层（长保留）
--
-- 百分比与负载用定点整数而不是 REAL：省一半空间，且避免浮点在聚合时的累积误差。
--   cpu_pct / gpu_util : 万分比 0..=10000
--   load1              : ×100
--   温度               : ×10 摄氏度
-- 内存存 total/free/available 三元组，两种口径（含不含 buff/cache）都能算。
-- ---------------------------------------------------------------------------
CREATE TABLE metrics_minute (
    server_id     INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    ts            INTEGER NOT NULL,          -- 分钟对齐的 unix 秒
    cpu_pct       INTEGER,
    cpu_pct_max   INTEGER,
    load1         INTEGER,
    mem_total     INTEGER,
    mem_free      INTEGER,
    mem_available INTEGER,
    swap_used     INTEGER,
    disk_used     INTEGER,
    disk_total    INTEGER,
    net_in_speed  INTEGER,                   -- B/s，窗口内平均
    net_out_speed INTEGER,
    net_in_peak   INTEGER,                   -- B/s，窗口内峰值
    net_out_peak  INTEGER,
    net_in_total  INTEGER,                   -- 网卡累计计数（原始值）
    net_out_total INTEGER,
    tcp_conn      INTEGER,
    udp_conn      INTEGER,
    proc_count    INTEGER,
    gpu_util      INTEGER,
    gpu_mem_used  INTEGER,
    gpu_temp      INTEGER,
    cpu_temp      INTEGER,
    PRIMARY KEY (server_id, ts)
);
-- 清理任务按 ts 扫；没有这个索引，每次 prune 都是全表扫
CREATE INDEX idx_metrics_minute_ts ON metrics_minute(ts);

CREATE TABLE metrics_hour (
    server_id     INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    ts            INTEGER NOT NULL,          -- 小时对齐
    cpu_pct       INTEGER,
    cpu_pct_max   INTEGER,
    load1         INTEGER,
    mem_total     INTEGER,
    mem_free      INTEGER,
    mem_available INTEGER,
    swap_used     INTEGER,
    disk_used     INTEGER,
    disk_total    INTEGER,
    net_in_speed  INTEGER,
    net_out_speed INTEGER,
    net_in_peak   INTEGER,
    net_out_peak  INTEGER,
    net_in_total  INTEGER,
    net_out_total INTEGER,
    tcp_conn      INTEGER,
    udp_conn      INTEGER,
    proc_count    INTEGER,
    gpu_util      INTEGER,
    gpu_mem_used  INTEGER,
    gpu_temp      INTEGER,
    cpu_temp      INTEGER,
    PRIMARY KEY (server_id, ts)
);
CREATE INDEX idx_metrics_hour_ts ON metrics_hour(ts);

-- ---------------------------------------------------------------------------
-- 延迟监控：三层降采样（raw 24h / 5m 7d / hour 30d）
--
-- 丢包率不存百分比，存 sent/recv 两个计数 —— 上卷时是简单求和，
-- 而不同样本数的百分比直接平均是错的。
-- rtt 单位微秒。
-- ---------------------------------------------------------------------------
CREATE TABLE ping_raw (
    server_id INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    task_id   INTEGER NOT NULL,
    ts        INTEGER NOT NULL,
    rtt_avg   INTEGER,
    rtt_min   INTEGER,
    rtt_max   INTEGER,
    sent      INTEGER NOT NULL,
    recv      INTEGER NOT NULL,
    PRIMARY KEY (server_id, task_id, ts)
);
CREATE INDEX idx_ping_raw_ts ON ping_raw(ts);

CREATE TABLE ping_5m (
    server_id INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    task_id   INTEGER NOT NULL,
    ts        INTEGER NOT NULL,
    rtt_avg   INTEGER,
    rtt_min   INTEGER,
    rtt_max   INTEGER,
    sent      INTEGER NOT NULL,
    recv      INTEGER NOT NULL,
    PRIMARY KEY (server_id, task_id, ts)
);
CREATE INDEX idx_ping_5m_ts ON ping_5m(ts);

CREATE TABLE ping_hour (
    server_id INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    task_id   INTEGER NOT NULL,
    ts        INTEGER NOT NULL,
    rtt_avg   INTEGER,
    rtt_min   INTEGER,
    rtt_max   INTEGER,
    sent      INTEGER NOT NULL,
    recv      INTEGER NOT NULL,
    PRIMARY KEY (server_id, task_id, ts)
);
CREATE INDEX idx_ping_hour_ts ON ping_hour(ts);

-- 上卷进度水位线：记录每层已经上卷到哪个时间点，避免重复计算与漏算
CREATE TABLE rollup_watermark (
    name   TEXT    PRIMARY KEY,
    ts     INTEGER NOT NULL
) WITHOUT ROWID;
