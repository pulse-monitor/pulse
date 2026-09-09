-- M5：账单、流量、套餐、汇率。
-- 算法与边界情况见 docs/06-business-rules.md。

-- ---------------------------------------------------------------------------
-- 账单（R4）
-- ---------------------------------------------------------------------------
CREATE TABLE server_billing (
    server_id         INTEGER PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
    price             REAL    NOT NULL,
    -- ISO 4217，如 USD / CNY
    currency          TEXT    NOT NULL,
    -- monthly|quarterly|semiannual|annual|biennial|triennial|onetime|custom
    cycle             TEXT    NOT NULL,
    -- cycle='custom' 时必填
    custom_cycle_days INTEGER,
    -- 当期起始；为空则由 expire_at 反推一个周期
    cycle_start_at    INTEGER,
    -- cycle='onetime'（买断）时为 NULL
    expire_at         INTEGER,
    auto_renew        INTEGER NOT NULL DEFAULT 0,
    purchased_at      INTEGER,
    remark            TEXT,
    updated_at        INTEGER NOT NULL
) WITHOUT ROWID;
-- 「即将到期」列表要按这个查
CREATE INDEX idx_billing_expire ON server_billing(expire_at) WHERE expire_at IS NOT NULL;

-- ---------------------------------------------------------------------------
-- 流量（R5）
--
-- **只存 in/out 两个方向，不存合计** —— 这样 calc_mode 改了之后
-- 历史周期也能按新口径重算，不用重来。
-- ---------------------------------------------------------------------------
CREATE TABLE server_traffic (
    server_id     INTEGER PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
    -- 当前统计周期起点（UTC 秒）
    period_start  INTEGER NOT NULL,
    in_bytes      INTEGER NOT NULL DEFAULT 0,
    out_bytes     INTEGER NOT NULL DEFAULT 0,
    -- 上次观测到的网卡累计值，用来算 delta。NULL = 还没建基线
    last_raw_in   INTEGER,
    last_raw_out  INTEGER,
    last_raw_at   INTEGER,
    -- 已经触发过的告警档位，JSON 数组。周期重置时清空
    alerted_pct   TEXT    NOT NULL DEFAULT '[]',
    updated_at    INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE traffic_history (
    server_id     INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    period_start  INTEGER NOT NULL,
    period_end    INTEGER NOT NULL,
    in_bytes      INTEGER NOT NULL,
    out_bytes     INTEGER NOT NULL,
    PRIMARY KEY (server_id, period_start)
) WITHOUT ROWID;

CREATE TABLE server_traffic_config (
    server_id   INTEGER PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
    -- NULL = 无限（前端显示 ♾️）
    limit_bytes INTEGER,
    -- sum|max|min|upload|download
    calc_mode   TEXT    NOT NULL DEFAULT 'sum',
    -- 1-31，31 表示「当月最后一天」
    reset_day   INTEGER NOT NULL DEFAULT 1,
    -- 覆盖面板时区；NULL = 用面板的
    timezone    TEXT,
    alert_pct   TEXT    NOT NULL DEFAULT '[80,95,100]'
) WITHOUT ROWID;

-- ---------------------------------------------------------------------------
-- 套餐模板（R16）：多台同款机器共享一条购买/测评链接，不用逐台填
-- ---------------------------------------------------------------------------
CREATE TABLE plans (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT NOT NULL,
    provider   TEXT,
    buy_url    TEXT,
    review_url TEXT,
    -- 参考价，新建服务器时预填
    price      REAL,
    currency   TEXT,
    cycle      TEXT,
    created_at INTEGER NOT NULL
);
ALTER TABLE servers ADD COLUMN plan_id INTEGER REFERENCES plans(id) ON DELETE SET NULL;

-- ---------------------------------------------------------------------------
-- 汇率（R15）：全部相对 USD
-- ---------------------------------------------------------------------------
CREATE TABLE exchange_rates (
    quote      TEXT PRIMARY KEY,
    rate       REAL NOT NULL,
    -- 'frankfurter' 自动拉取 | 'manual' 人工填写。
    -- **manual 优先级更高** —— 用于源不支持的货币（RUB/TWD/VND/UAH/ARS/AED…）
    source     TEXT NOT NULL,
    -- 源返回的日期
    as_of      TEXT NOT NULL,
    fetched_at INTEGER NOT NULL
) WITHOUT ROWID;
