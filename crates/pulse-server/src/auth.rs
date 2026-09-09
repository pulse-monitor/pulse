//! 认证：密码、令牌、JWT、登录限流。
//!
//! 设计约束来自 ：
//! - 管理员密码用 **argon2id**，参数按 OWASP 推荐
//! - agent token 与 refresh token 在数据库里**只存 SHA-256**，明文只出现一次
//! - 登录限流是有状态但**纯逻辑**的，时间从参数传入，因此可完整单测

use std::net::IpAddr;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use dashmap::DashMap;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation};
use rand::TryRngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// access token 有效期。短是故意的 —— 泄露的窗口小，
/// 续期由 HttpOnly 的 refresh cookie 承担。
pub const ACCESS_TTL: Duration = Duration::from_secs(15 * 60);
/// refresh token 有效期。
pub const REFRESH_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

// ---------------------------------------------------------------------------
// 密码
// ---------------------------------------------------------------------------

/// OWASP 推荐的 argon2id 参数：m=19456 KiB, t=2, p=1。
fn argon2() -> Argon2<'static> {
    let params = Params::new(19_456, 2, 1, None).expect("argon2 参数应当合法");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

pub fn hash_password(plain: &str) -> Result<String> {
    // 盐与令牌用同一个随机源（OS CSPRNG），少一条依赖路径也少一处可能配错的地方
    let salt = SaltString::encode_b64(&random_bytes::<16>()?)
        .map_err(|e| anyhow::anyhow!("生成盐失败: {e}"))?;
    Ok(argon2()
        .hash_password(plain.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2 计算失败: {e}"))?
        .to_string())
}

/// 校验密码。**失败与格式错误都返回 false**，不区分 —— 区分了就是个预言机。
pub fn verify_password(plain: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    argon2().verify_password(plain.as_bytes(), &parsed).is_ok()
}

// ---------------------------------------------------------------------------
// 令牌
// ---------------------------------------------------------------------------

/// 生成一个 256 位的随机令牌，base64url 无填充。
///
/// 用于 agent token 与 refresh token。熵取自 OS CSPRNG。
pub fn generate_token() -> Result<String> {
    Ok(base64url(&random_bytes::<32>()?))
}

/// 从 OS CSPRNG 取 N 字节。全项目唯一的随机源入口。
fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    rand::rngs::OsRng
        .try_fill_bytes(&mut buf)
        .context("读取系统随机数失败")?;
    Ok(buf)
}

fn base64url(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let idx = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        for (i, &j) in idx.iter().enumerate() {
            // 无填充：最后一组不足时少输出对应的字符
            if i <= c.len() {
                out.push(A[j as usize] as char);
            }
        }
    }
    out
}

/// 令牌的存储形式。数据库里只存这个，明文永不落盘。
pub fn token_hash(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    hex::encode(h.finalize())
}

// ---------------------------------------------------------------------------
// JWT
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenKind {
    Access,
    Refresh,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// 用户名
    pub sub: String,
    pub exp: i64,
    pub iat: i64,
    pub typ: TokenKind,
    /// 令牌唯一标识，refresh 的吊销以它为键
    pub jti: String,
}

#[derive(Clone)]
pub struct Jwt {
    enc: EncodingKey,
    dec: DecodingKey,
}

impl Jwt {
    pub fn new(secret: &[u8]) -> Self {
        Self {
            enc: EncodingKey::from_secret(secret),
            dec: DecodingKey::from_secret(secret),
        }
    }

    pub fn issue(&self, user: &str, kind: TokenKind, now: i64) -> Result<(String, String)> {
        let ttl = match kind {
            TokenKind::Access => ACCESS_TTL,
            TokenKind::Refresh => REFRESH_TTL,
        };
        let jti = generate_token()?;
        let claims = Claims {
            sub: user.to_string(),
            iat: now,
            exp: now + ttl.as_secs() as i64,
            typ: kind,
            jti: jti.clone(),
        };
        let token = jsonwebtoken::encode(&Header::default(), &claims, &self.enc)
            .context("签发 JWT 失败")?;
        Ok((token, jti))
    }

    /// 校验并解出 claims。**类型不符也算失败** ——
    /// 否则 refresh token 能直接当 access token 用，短有效期就白设了。
    pub fn verify(&self, token: &str, want: TokenKind) -> Result<Claims> {
        let mut v = Validation::default();
        v.validate_exp = true;
        v.leeway = 5;
        let data =
            jsonwebtoken::decode::<Claims>(token, &self.dec, &v).context("JWT 无效或已过期")?;
        if data.claims.typ != want {
            bail!("令牌类型不符：期望 {want:?}，实际 {:?}", data.claims.typ);
        }
        Ok(data.claims)
    }
}

// ---------------------------------------------------------------------------
// 登录限流
// ---------------------------------------------------------------------------

/// 每分钟允许的尝试次数。
const RATE_WINDOW: Duration = Duration::from_secs(60);
const RATE_MAX: u32 = 5;
/// 连续失败多少次后锁定，以及锁多久。
const LOCK_AFTER_FAILURES: u32 = 10;
const LOCK_DURATION: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Copy)]
struct Attempts {
    window_start: Instant,
    in_window: u32,
    consecutive_failures: u32,
    locked_until: Option<Instant>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Block {
    /// 频率超限，稍后再试
    TooFast { retry_after: Duration },
    /// 连续失败过多，已锁定
    Locked { retry_after: Duration },
}

impl Block {
    pub fn retry_after(&self) -> Duration {
        match self {
            Block::TooFast { retry_after } | Block::Locked { retry_after } => *retry_after,
        }
    }
}

/// 按来源 IP 的登录限流。
///
/// 时间从参数传入而不是内部取 `Instant::now()` —— 这样整套状态机可以在
/// 单元测试里精确推进，不用 sleep。
#[derive(Default)]
pub struct LoginLimiter {
    inner: DashMap<IpAddr, Attempts>,
}

impl LoginLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// 尝试登录前调用。返回 `Err` 表示应当直接拒绝，不要去验密码。
    pub fn check(&self, ip: IpAddr, now: Instant) -> Result<(), Block> {
        let mut e = self.inner.entry(ip).or_insert(Attempts {
            window_start: now,
            in_window: 0,
            consecutive_failures: 0,
            locked_until: None,
        });

        if let Some(until) = e.locked_until {
            if now < until {
                return Err(Block::Locked {
                    retry_after: until - now,
                });
            }
            // 锁定期满：解锁并清零，给一次干净的重来机会
            e.locked_until = None;
            e.consecutive_failures = 0;
        }

        if now.duration_since(e.window_start) >= RATE_WINDOW {
            e.window_start = now;
            e.in_window = 0;
        }
        if e.in_window >= RATE_MAX {
            let elapsed = now.duration_since(e.window_start);
            return Err(Block::TooFast {
                retry_after: RATE_WINDOW - elapsed,
            });
        }
        e.in_window += 1;
        Ok(())
    }

    pub fn record_failure(&self, ip: IpAddr, now: Instant) {
        if let Some(mut e) = self.inner.get_mut(&ip) {
            e.consecutive_failures += 1;
            if e.consecutive_failures >= LOCK_AFTER_FAILURES {
                e.locked_until = Some(now + LOCK_DURATION);
            }
        }
    }

    pub fn record_success(&self, ip: IpAddr) {
        // 成功后清零，但**不清 in_window** —— 否则拿一个正确密码就能
        // 无限次刷接口
        if let Some(mut e) = self.inner.get_mut(&ip) {
            e.consecutive_failures = 0;
            e.locked_until = None;
        }
    }

    /// 清理长期没有活动的条目，避免 map 无界增长。
    pub fn sweep(&self, now: Instant) {
        self.inner.retain(|_, a| {
            a.locked_until.is_some_and(|u| now < u)
                || now.duration_since(a.window_start) < RATE_WINDOW * 10
        });
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.len()
    }
}

// ---------------------------------------------------------------------------
// 密钥
// ---------------------------------------------------------------------------

/// 读取或生成服务端密钥。
///
/// 优先 `PULSE_SECRET_KEY`（hex），否则用 `path` 指向的文件；文件不存在则生成。
/// **这个密钥丢了等于所有渠道凭据无法解密**，
/// 所以生成后要提醒用户单独备份。
pub fn load_or_create_secret(path: &std::path::Path) -> Result<Vec<u8>> {
    if let Ok(hex_key) = std::env::var("PULSE_SECRET_KEY") {
        let k = hex::decode(hex_key.trim()).context("PULSE_SECRET_KEY 不是合法的 hex")?;
        if k.len() < 32 {
            bail!("PULSE_SECRET_KEY 至少需要 32 字节（64 个 hex 字符）");
        }
        return Ok(k);
    }
    if let Ok(s) = std::fs::read_to_string(path) {
        let k = hex::decode(s.trim()).context("secret.key 内容不是合法的 hex")?;
        if k.len() >= 32 {
            return Ok(k);
        }
        bail!(
            "{} 的内容太短，疑似损坏；删掉它会重新生成，但已有的会话与加密数据将失效",
            path.display()
        );
    }

    let buf = random_bytes::<32>()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    std::fs::write(path, hex::encode(buf))
        .with_context(|| format!("写入 {} 失败", path.display()))?;
    restrict_permissions(path);
    Ok(buf.to_vec())
}

#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &std::path::Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip() -> IpAddr {
        "203.0.113.7".parse().unwrap()
    }

    #[test]
    fn password_roundtrip() {
        let h = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &h));
        assert!(!verify_password("wrong", &h));
        // 同一密码两次哈希必须不同（盐生效），否则能看出哪些账号同密码
        let h2 = hash_password("correct horse battery staple").unwrap();
        assert_ne!(h, h2);
        assert!(verify_password("correct horse battery staple", &h2));
    }

    #[test]
    fn verify_rejects_malformed_hash_without_panicking() {
        // 数据库被改坏时不能 panic，也不能放行
        for bad in ["", "garbage", "$argon2id$v=19$bogus", "$2b$10$notargon2"] {
            assert!(!verify_password("anything", bad), "不该接受 {bad:?}");
        }
    }

    #[test]
    fn hash_does_not_leak_the_password() {
        let h = hash_password("hunter2").unwrap();
        assert!(!h.contains("hunter2"));
        assert!(h.starts_with("$argon2id$"));
    }

    #[test]
    fn tokens_are_unique_and_url_safe() {
        let a = generate_token().unwrap();
        let b = generate_token().unwrap();
        assert_ne!(a, b);
        assert!(
            a.len() >= 42,
            "256 位应当至少 43 个 base64url 字符，实际 {}",
            a.len()
        );
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "必须是 URL 安全字符，实际 {a}"
        );
    }

    #[test]
    fn token_hash_is_stable_and_hides_the_token() {
        let t = "super-secret-token";
        assert_eq!(token_hash(t), token_hash(t));
        assert_ne!(token_hash(t), token_hash("other"));
        assert_eq!(token_hash(t).len(), 64);
        assert!(!token_hash(t).contains("secret"));
    }

    #[test]
    fn jwt_roundtrip_and_kind_is_enforced() {
        let j = Jwt::new(b"0123456789abcdef0123456789abcdef");
        let now = 1_800_000_000;
        let (access, _) = j.issue("admin", TokenKind::Access, now).unwrap();

        let c = j.verify(&access, TokenKind::Access).unwrap();
        assert_eq!(c.sub, "admin");
        assert_eq!(c.exp, now + ACCESS_TTL.as_secs() as i64);

        // refresh token 不能当 access 用，否则 15 分钟的短有效期就白设了
        let (refresh, _) = j.issue("admin", TokenKind::Refresh, now).unwrap();
        assert!(j.verify(&refresh, TokenKind::Access).is_err());
        assert!(j.verify(&access, TokenKind::Refresh).is_err());
    }

    #[test]
    fn jwt_rejects_wrong_secret_and_expired() {
        let j = Jwt::new(b"0123456789abcdef0123456789abcdef");
        let other = Jwt::new(b"ffffffffffffffffffffffffffffffff");
        let now = 1_800_000_000;
        let (t, _) = j.issue("admin", TokenKind::Access, now).unwrap();
        assert!(
            other.verify(&t, TokenKind::Access).is_err(),
            "换密钥必须验不过"
        );

        // 签发一个早已过期的
        let (old, _) = j.issue("admin", TokenKind::Access, 1_000_000_000).unwrap();
        assert!(j.verify(&old, TokenKind::Access).is_err(), "过期必须验不过");
    }

    #[test]
    fn jwt_jti_is_unique_per_token() {
        // refresh 的吊销以 jti 为键，重复就会误伤
        let j = Jwt::new(b"0123456789abcdef0123456789abcdef");
        let (_, a) = j.issue("admin", TokenKind::Refresh, 0).unwrap();
        let (_, b) = j.issue("admin", TokenKind::Refresh, 0).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn limiter_allows_up_to_the_rate_then_blocks() {
        let l = LoginLimiter::new();
        let t0 = Instant::now();
        for i in 0..RATE_MAX {
            assert!(l.check(ip(), t0).is_ok(), "第 {i} 次应当放行");
        }
        match l.check(ip(), t0) {
            Err(Block::TooFast { retry_after }) => assert!(retry_after <= RATE_WINDOW),
            other => panic!("第 6 次应当被限流，实际 {other:?}"),
        }
    }

    #[test]
    fn limiter_window_rolls_over() {
        let l = LoginLimiter::new();
        let t0 = Instant::now();
        for _ in 0..RATE_MAX {
            l.check(ip(), t0).unwrap();
        }
        assert!(l.check(ip(), t0).is_err());
        // 窗口过去之后重新放行
        assert!(l.check(ip(), t0 + RATE_WINDOW).is_ok());
    }

    #[test]
    fn limiter_locks_after_repeated_failures() {
        let l = LoginLimiter::new();
        let mut t = Instant::now();
        for _ in 0..LOCK_AFTER_FAILURES {
            // 每次都换窗口，避免被频率限流挡住，专测失败计数
            l.check(ip(), t).unwrap();
            l.record_failure(ip(), t);
            t += RATE_WINDOW;
        }
        match l.check(ip(), t) {
            Err(Block::Locked { retry_after }) => {
                assert!(retry_after <= LOCK_DURATION);
            }
            other => panic!("连续失败 {LOCK_AFTER_FAILURES} 次后应当锁定，实际 {other:?}"),
        }
        // 锁定期满后自动解锁
        assert!(l.check(ip(), t + LOCK_DURATION).is_ok());
    }

    #[test]
    fn successful_login_clears_failure_streak_but_not_rate_window() {
        let l = LoginLimiter::new();
        let t = Instant::now();
        for _ in 0..3 {
            l.check(ip(), t).unwrap();
            l.record_failure(ip(), t);
        }
        l.record_success(ip());
        // 失败streak 清零了，但本窗口已用掉 3 次，只剩 2 次 ——
        // 否则拿一个正确密码就能无限刷接口
        assert!(l.check(ip(), t).is_ok());
        assert!(l.check(ip(), t).is_ok());
        assert!(l.check(ip(), t).is_err(), "成功登录不应重置频率窗口");
    }

    #[test]
    fn limiter_isolates_different_ips() {
        let l = LoginLimiter::new();
        let t = Instant::now();
        let a: IpAddr = "203.0.113.7".parse().unwrap();
        let b: IpAddr = "203.0.113.8".parse().unwrap();
        for _ in 0..RATE_MAX {
            l.check(a, t).unwrap();
        }
        assert!(l.check(a, t).is_err());
        assert!(l.check(b, t).is_ok(), "一个 IP 被限流不能影响另一个");
    }

    #[test]
    fn sweep_bounds_memory_but_keeps_locked_entries() {
        let l = LoginLimiter::new();
        let t = Instant::now();
        for i in 0..50u8 {
            let ip: IpAddr = format!("203.0.113.{i}").parse().unwrap();
            l.check(ip, t).unwrap();
        }
        // 锁住其中一个
        for _ in 0..LOCK_AFTER_FAILURES {
            l.record_failure(ip(), t);
        }
        assert_eq!(l.len(), 50);
        l.sweep(t + RATE_WINDOW * 11);
        assert_eq!(l.len(), 1, "只应保留仍在锁定期内的条目");
    }

    #[test]
    fn secret_is_generated_once_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("secret.key");
        let a = load_or_create_secret(&p).unwrap();
        let b = load_or_create_secret(&p).unwrap();
        assert_eq!(a, b, "重启后必须读回同一个密钥，否则所有会话失效");
        assert!(a.len() >= 32);
    }

    #[test]
    fn corrupt_secret_file_fails_loudly() {
        // 静默重新生成会让所有已签发的令牌与加密数据无声失效
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("secret.key");
        std::fs::write(&p, "ab").unwrap();
        assert!(load_or_create_secret(&p).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn generated_secret_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("secret.key");
        load_or_create_secret(&p).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "密钥文件不能对其他用户可读，实际 {mode:o}");
    }
}
