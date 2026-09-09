//! 渠道凭据的加密存储。
//!
//! Telegram bot token、SMTP 密码、企微/飞书的 webhook key 都是**能直接
//! 用来发消息的凭据**，明文落库等于面板数据库一泄露就全丢。
//!
//! 用 AES-256-GCM：密钥来自 `PULSE_SECRET_KEY` 或 `data/secret.key`
//! （与 JWT 同一把，见 `auth::load_or_create_secret`）。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{bail, Context, Result};
use rand::TryRngCore;

/// GCM 的 nonce 长度。**每次加密都必须换一个** ——
/// 同一把密钥下 nonce 重用会直接暴露明文异或。
const NONCE_LEN: usize = 12;

/// 密文的存储格式：`nonce(12) || ciphertext+tag`。
///
/// nonce 跟着密文一起存是标准做法 —— 它不是秘密，只需要不重复。
pub fn encrypt(secret: &[u8], plain: &str) -> Result<Vec<u8>> {
    let cipher = build(secret)?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rngs::OsRng
        .try_fill_bytes(&mut nonce_bytes)
        .context("读取系统随机数失败")?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    let mut out = nonce_bytes.to_vec();
    out.extend(
        cipher
            .encrypt(nonce, plain.as_bytes())
            .map_err(|_| anyhow::anyhow!("加密失败"))?,
    );
    Ok(out)
}

pub fn decrypt(secret: &[u8], blob: &[u8]) -> Result<String> {
    if blob.len() <= NONCE_LEN {
        bail!("密文过短，疑似损坏");
    }
    let (nonce_bytes, ct) = blob.split_at(NONCE_LEN);
    let plain = build(secret)?
        .decrypt(Nonce::from_slice(nonce_bytes), ct)
        // 不区分「密钥错」和「数据被篡改」—— 区分了就是个预言机
        .map_err(|_| anyhow::anyhow!("解密失败：密钥不对，或数据已损坏/被篡改"))?;
    String::from_utf8(plain).context("解密结果不是合法 UTF-8")
}

fn build(secret: &[u8]) -> Result<Aes256Gcm> {
    if secret.len() < 32 {
        bail!("密钥至少需要 32 字节");
    }
    Ok(Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&secret[..32])))
}

/// 把凭据里的敏感字段替换成掩码，用于回显给前端。
///
/// **绝不把明文凭据发回浏览器** —— 用户改配置时只需要确认「填过了」，
/// 不需要看到内容。
pub fn mask(s: &str) -> String {
    let n = s.chars().count();
    if n == 0 {
        return String::new();
    }
    if n <= 8 {
        return "•".repeat(n);
    }
    let head: String = s.chars().take(3).collect();
    let tail: String = s.chars().skip(n - 3).collect();
    format!("{head}{}{tail}", "•".repeat(6))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

    #[test]
    fn roundtrip() {
        let secrets = [
            "1234567890:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw",
            "很长的中文密码含 emoji 🔑",
            "",
        ];
        for s in secrets {
            let blob = encrypt(KEY, s).unwrap();
            assert_eq!(decrypt(KEY, &blob).unwrap(), s);
        }
    }

    #[test]
    fn ciphertext_never_contains_the_plaintext() {
        let token = "SUPER-SECRET-BOT-TOKEN";
        let blob = encrypt(KEY, token).unwrap();
        let hay = String::from_utf8_lossy(&blob);
        assert!(!hay.contains("SUPER"), "密文里不该出现明文片段");
        assert!(!hay.contains(token));
    }

    #[test]
    fn same_plaintext_encrypts_differently_each_time() {
        // nonce 必须每次都换：重用会让同一把密钥下的两段密文异或出明文
        let a = encrypt(KEY, "same").unwrap();
        let b = encrypt(KEY, "same").unwrap();
        assert_ne!(a, b, "两次加密结果必须不同（nonce 生效）");
        assert_ne!(a[..NONCE_LEN], b[..NONCE_LEN], "nonce 必须不同");
        // 但都能解回同一个明文
        assert_eq!(decrypt(KEY, &a).unwrap(), decrypt(KEY, &b).unwrap());
    }

    #[test]
    fn wrong_key_fails_instead_of_returning_garbage() {
        let blob = encrypt(KEY, "secret").unwrap();
        let other = b"ffffffffffffffffffffffffffffffff";
        assert!(decrypt(other, &blob).is_err(), "换密钥必须解不出来");
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        // GCM 是带认证的：改一个 bit 就该失败，而不是解出被篡改的内容
        let mut blob = encrypt(KEY, "transfer 100 to alice").unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
        assert!(decrypt(KEY, &blob).is_err());

        // 改 nonce 同样失败
        let mut blob2 = encrypt(KEY, "x").unwrap();
        blob2[0] ^= 0xff;
        assert!(decrypt(KEY, &blob2).is_err());
    }

    #[test]
    fn truncated_blob_fails_without_panicking() {
        for n in 0..=NONCE_LEN {
            assert!(
                decrypt(KEY, &vec![0u8; n]).is_err(),
                "长度 {n} 应当报错而不是 panic"
            );
        }
    }

    #[test]
    fn short_key_is_rejected() {
        assert!(encrypt(b"tooshort", "x").is_err());
        assert!(decrypt(b"tooshort", &[0u8; 40]).is_err());
    }

    #[test]
    fn mask_keeps_enough_to_recognise_but_not_to_use() {
        assert_eq!(mask(""), "");
        assert_eq!(mask("abc"), "•••");
        assert_eq!(mask("12345678"), "••••••••");
        let m = mask("1234567890:AAHdqTcvCH1vGWJx");
        assert!(m.starts_with("123") && m.ends_with("WJx"));
        assert!(!m.contains("AAHdqTcv"), "中段必须被遮住");
    }
}
