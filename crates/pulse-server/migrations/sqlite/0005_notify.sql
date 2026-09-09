-- M6：通知渠道、规则、告警状态机。

-- ---------------------------------------------------------------------------
-- 渠道
-- ---------------------------------------------------------------------------
CREATE TABLE notification_channels (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT    NOT NULL,
    -- email | telegram | wecom | lark
    kind       TEXT    NOT NULL,
    -- **AES-256-GCM 加密后的 JSON**。里面是 bot token / SMTP 密码 /
    -- webhook key 这类能直接用来发消息的凭据，明文落库等于数据库一泄露就全丢
    config_enc BLOB    NOT NULL,
    enabled    INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL
);

-- ---------------------------------------------------------------------------
-- 规则
-- ---------------------------------------------------------------------------
CREATE TABLE notification_rules (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT    NOT NULL,
    -- JSON 数组：["server_offline","cpu_high",…]
    event_kinds TEXT    NOT NULL,
    -- JSON 数组：渠道 id
    channel_ids TEXT    NOT NULL,
    -- all | group | servers
    scope_kind  TEXT    NOT NULL DEFAULT 'all',
    scope_ids   TEXT    NOT NULL DEFAULT '[]',
    -- JSON：阈值等参数，如 {"cpu_pct":90,"disk_pct":90}
    params      TEXT    NOT NULL DEFAULT '{}',
    -- 条件需持续多久才触发（防网络抖动）
    duration_s  INTEGER NOT NULL DEFAULT 90,
    -- 触发后多久才允许再次提醒（防刷屏）
    cooldown_s  INTEGER NOT NULL DEFAULT 3600,
    notify_resolve INTEGER NOT NULL DEFAULT 1,
    -- 自定义模板，留空用默认
    title_tpl   TEXT,
    body_tpl    TEXT,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  INTEGER NOT NULL
);

-- ---------------------------------------------------------------------------
-- 告警状态机的持久化 + 历史
--
-- **这张表是「重启不重复轰炸、也不漏掉已触发的告警」的关键**。
-- 唯一索引保证同一 (规则, 机器, 事件) 只有一条活跃记录。
-- ---------------------------------------------------------------------------
CREATE TABLE notification_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    rule_id     INTEGER NOT NULL REFERENCES notification_rules(id) ON DELETE CASCADE,
    -- 全局事件（如「面板自身」）时为 NULL
    server_id   INTEGER REFERENCES servers(id) ON DELETE CASCADE,
    event_kind  TEXT    NOT NULL,
    -- pending | firing | resolved
    state       TEXT    NOT NULL,
    first_at    INTEGER NOT NULL,
    fired_at    INTEGER,
    resolved_at INTEGER,
    last_notified_at INTEGER,
    -- 触发时的现场值，用于历史查看
    payload     TEXT
);

-- 同一 (规则, 机器, 事件) 只能有一条活跃记录。
-- SQLite 的部分唯一索引里 NULL 互不相等，所以 server_id 为 NULL 的
-- 全局事件用 -1 占位而不是 NULL（见 store 层）。
CREATE UNIQUE INDEX idx_ne_active
    ON notification_events(rule_id, server_id, event_kind)
    WHERE state != 'resolved';

CREATE INDEX idx_ne_history ON notification_events(first_at DESC);
