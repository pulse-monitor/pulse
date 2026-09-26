-- M6：refresh 会话追踪。
--
-- 0002 建 revoked_token 时注释就写了「退出登录 / 改密码时把仍在有效期内的
-- token 拉黑」，但签发 refresh 时从没记过 jti —— 改密码时根本不知道
-- 「该管理员未过期的 refresh token」是哪些，这张表补上这条记录。
--
-- 只记 SHA-256(jti)：这张表泄露了也拿不到可用的 token，
-- 与 revoked_token 的口径一致。

CREATE TABLE refresh_session (
    token_hash TEXT    PRIMARY KEY,  -- SHA-256(jti)
    admin_id   INTEGER NOT NULL REFERENCES admin_user(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL      -- refresh 的原始有效期，过期后可清理
) WITHOUT ROWID;
CREATE INDEX idx_refresh_session_admin ON refresh_session(admin_id);
CREATE INDEX idx_refresh_session_expires ON refresh_session(expires_at);
