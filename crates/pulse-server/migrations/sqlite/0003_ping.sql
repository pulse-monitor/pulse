-- M4：延迟监控任务。
-- 三层结果表（ping_raw / ping_5m / ping_hour）在 0001 里已经建好。

CREATE TABLE ping_tasks (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT    NOT NULL,
    -- tcp | icmp | http
    kind       TEXT    NOT NULL,
    -- tcp: 主机名或 IP；http: 完整 URL
    host       TEXT    NOT NULL,
    -- 仅 tcp 用
    port       INTEGER,
    -- 仅 http 用：期望的状态码，NULL 表示只要能连上就算通
    expect_status INTEGER,

    interval_s INTEGER NOT NULL DEFAULT 60,
    -- 每轮发几个包，用来算丢包率
    packets    INTEGER NOT NULL DEFAULT 3,
    timeout_ms INTEGER NOT NULL DEFAULT 3000,

    -- 作用范围：all | group | servers
    scope_kind TEXT    NOT NULL DEFAULT 'all',
    -- JSON 数组：group 时是分组 id，servers 时是服务器 id
    scope_ids  TEXT    NOT NULL DEFAULT '[]',

    enabled    INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL
);

-- 只有启用的任务需要下发，加个部分索引
CREATE INDEX idx_ping_tasks_enabled ON ping_tasks(enabled) WHERE enabled = 1;
