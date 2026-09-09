-- M3：认证、服务器管理、运行期配置持久化。
-- 完整 schema 设计见 docs/02-data-model.md。

-- ---------------------------------------------------------------------------
-- 管理员
-- ---------------------------------------------------------------------------
CREATE TABLE admin_user (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    username      TEXT    NOT NULL UNIQUE,
    -- argon2id。绝不存明文，也不存可逆密文
    password_hash TEXT    NOT NULL,
    totp_secret   TEXT,                       -- NULL = 未启用二次验证
    created_at    INTEGER NOT NULL,
    last_login_at INTEGER
);

-- refresh token 的吊销表。退出登录 / 改密码时把仍在有效期内的 token 拉黑。
-- 只存 SHA-256，泄露这张表也拿不到可用的 token。
CREATE TABLE revoked_token (
    token_hash TEXT    PRIMARY KEY,
    expires_at INTEGER NOT NULL              -- 过了原始有效期就可以清理掉
) WITHOUT ROWID;
CREATE INDEX idx_revoked_expires ON revoked_token(expires_at);

-- ---------------------------------------------------------------------------
-- 站点设置：key-value，值是 JSON
-- ---------------------------------------------------------------------------
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
) WITHOUT ROWID;

-- ---------------------------------------------------------------------------
-- 分组：M3 只建表（servers 要外键引用），CRUD 在 M5
-- ---------------------------------------------------------------------------
CREATE TABLE groups (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT NOT NULL UNIQUE,
    color      TEXT,
    icon       TEXT,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);

-- ---------------------------------------------------------------------------
-- servers 扩展
--
-- SQLite 的 ALTER TABLE 只能一次加一列，且不支持给已有表加 UNIQUE 约束 ——
-- token_hash 的唯一性用独立的唯一索引来保证。
-- ---------------------------------------------------------------------------

-- 凭据：数据库里**只存 SHA-256**，明文只在创建时返回一次。
-- 面板数据库泄露 ≠ 所有 agent 凭据泄露。
ALTER TABLE servers ADD COLUMN token_hash TEXT;
CREATE UNIQUE INDEX idx_servers_token ON servers(token_hash) WHERE token_hash IS NOT NULL;

ALTER TABLE servers ADD COLUMN group_id   INTEGER REFERENCES groups(id) ON DELETE SET NULL;
ALTER TABLE servers ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0;
ALTER TABLE servers ADD COLUMN hidden     INTEGER NOT NULL DEFAULT 0;  -- 1 = 不在公开页展示
ALTER TABLE servers ADD COLUMN note       TEXT;

-- R16：购买同款 / 测评链接
ALTER TABLE servers ADD COLUMN buy_url    TEXT;
ALTER TABLE servers ADD COLUMN review_url TEXT;

-- 地理位置。GeoIP 自动填，后台可覆盖；location_manual=1 后 GeoIP 不再改写
ALTER TABLE servers ADD COLUMN country_code    TEXT;
ALTER TABLE servers ADD COLUMN region          TEXT;
ALTER TABLE servers ADD COLUMN latitude        REAL;
ALTER TABLE servers ADD COLUMN longitude       REAL;
ALTER TABLE servers ADD COLUMN location_manual INTEGER NOT NULL DEFAULT 0;
ALTER TABLE servers ADD COLUMN asn             TEXT;
ALTER TABLE servers ADD COLUMN isp             TEXT;
-- agent 连入的源 IP。**默认不对公开页暴露**（docs/08-security.md）
ALTER TABLE servers ADD COLUMN last_ip         TEXT;

-- agent Hello 上报的静态信息
ALTER TABLE servers ADD COLUMN os             TEXT;
ALTER TABLE servers ADD COLUMN kernel         TEXT;
ALTER TABLE servers ADD COLUMN arch           TEXT;
ALTER TABLE servers ADD COLUMN virtualization TEXT;
ALTER TABLE servers ADD COLUMN cpu_model      TEXT;
ALTER TABLE servers ADD COLUMN cpu_cores      INTEGER;
ALTER TABLE servers ADD COLUMN mem_total      INTEGER;
ALTER TABLE servers ADD COLUMN swap_total     INTEGER;
ALTER TABLE servers ADD COLUMN disk_total     INTEGER;
ALTER TABLE servers ADD COLUMN agent_version  TEXT;
ALTER TABLE servers ADD COLUMN boot_at        INTEGER;
-- agent 声明的能力，JSON。前端据此隐藏本机采不到的字段
ALTER TABLE servers ADD COLUMN capabilities   TEXT;

-- ---------------------------------------------------------------------------
-- 运行期配置（docs/04-agent.md 的「安装选项三分法」之运行期那一类）
--
-- 后台改完 WS 下发，≤2 秒生效，不用碰机器。
-- 每台一行；没有行时 agent 用协议里的默认值。
-- ---------------------------------------------------------------------------
CREATE TABLE server_runtime_config (
    server_id         INTEGER PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
    interval_s        INTEGER NOT NULL DEFAULT 2,
    net_include       TEXT    NOT NULL DEFAULT '[]',   -- JSON 数组
    net_exclude       TEXT    NOT NULL DEFAULT '[]',
    disk_include      TEXT    NOT NULL DEFAULT '[]',
    disk_exclude      TEXT    NOT NULL DEFAULT '[]',
    gpu_enabled       INTEGER NOT NULL DEFAULT 0,
    report_temps      INTEGER NOT NULL DEFAULT 1,
    report_conn_count INTEGER NOT NULL DEFAULT 1,
    updated_at        INTEGER NOT NULL
) WITHOUT ROWID;
