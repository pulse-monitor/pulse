//! 签名与摘要校验。**纯函数，可完整测到底。**
//!
//! 对应 [] 里的第 2、3 道防线：
//! minisign 验签 + SHA-256 清单校验（清单本身在签名覆盖范围内）。

use sha2::{Digest, Sha256};

/// 校验失败的原因。**每一种都必须导致放弃更新**，没有任何一种是可以「继续」的。
#[derive(Debug, PartialEq, Eq)]
pub enum Reject {
    /// 编译期没内置公钥 —— 自更新功能整个关闭
    NoPubkey,
    /// 内置的公钥本身格式不对（构建配置错了）
    BadPubkey,
    /// 签名文件格式不对
    BadSignature,
    /// 签名与清单内容不匹配 —— **这是被投毒时最可能看到的**
    SignatureMismatch,
    /// 清单里没有本平台这一行
    NotInManifest,
    /// 下载到的二进制摘要与清单不符
    DigestMismatch,
}

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Reject::NoPubkey => "未内置更新公钥，自更新已关闭",
            Reject::BadPubkey => "内置的更新公钥格式不对（构建配置有误）",
            Reject::BadSignature => "签名文件格式不对",
            Reject::SignatureMismatch => "签名校验不通过 —— 更新包可能被篡改",
            Reject::NotInManifest => "清单里没有本平台对应的条目",
            Reject::DigestMismatch => "二进制摘要与清单不符 —— 更新包可能被篡改",
        };
        f.write_str(s)
    }
}

/// 用内置公钥验证清单的 minisign 签名。
///
/// `pubkey` 是 minisign 的公钥文本（`minisign -G` 产出的那两行，或只有 base64 那一行）。
/// **空公钥一律拒绝**，不做「没配就跳过校验」的处理 —— 那等于把第 2 道防线关掉。
pub fn verify_manifest(pubkey: &str, manifest: &[u8], signature: &str) -> Result<(), Reject> {
    let pubkey = pubkey.trim();
    if pubkey.is_empty() {
        return Err(Reject::NoPubkey);
    }
    let pk = minisign_verify::PublicKey::decode(pubkey)
        .or_else(|_| minisign_verify::PublicKey::from_base64(pubkey))
        .map_err(|_| Reject::BadPubkey)?;
    let sig = minisign_verify::Signature::decode(signature).map_err(|_| Reject::BadSignature)?;
    // 第三个参数是 allow_legacy：不允许 —— 旧格式用的是 pure Ed25519，
    // 没有把「被签的是什么文件」绑进去
    pk.verify(manifest, &sig, false)
        .map_err(|_| Reject::SignatureMismatch)
}

/// 校验下载到的二进制：摘要必须与（已验签的）清单里的那一行一致。
pub fn verify_binary(manifest: &str, filename: &str, bytes: &[u8]) -> Result<(), Reject> {
    let want = super::manifest::sha256_of(manifest, filename).ok_or(Reject::NotInManifest)?;
    let got: [u8; 32] = Sha256::digest(bytes).into();
    // 摘要比较用常量时间没有意义（两边都是公开值），但**必须是全等比较**
    if got != want {
        return Err(Reject::DigestMismatch);
    }
    Ok(())
}

/// 发布产物里本平台那一行的文件名。构建期由 `build.rs` 注入目标三元组。
pub fn asset_name() -> String {
    let target = env!("PULSE_TARGET");
    if target.contains("windows") {
        format!("pulse-agent-{target}.exe")
    } else {
        format!("pulse-agent-{target}")
    }
}

/// 编译期内置的公钥。空 = 自更新关闭。
pub fn builtin_pubkey() -> &'static str {
    env!("PULSE_UPDATE_PUBKEY")
}

#[cfg(test)]
mod tests {
    use super::*;

    // 用 minisign 真实生成的一组测试数据（私钥只用于生成这些常量，未入库）。
    // 见 tools/gen-test-keys.sh
    const PUBKEY: &str = include_str!("testdata/test.pub");
    const MANIFEST: &[u8] = include_bytes!("testdata/SHA256SUMS");
    const SIG: &str = include_str!("testdata/SHA256SUMS.minisig");

    #[test]
    fn accepts_a_genuine_signature() {
        assert_eq!(verify_manifest(PUBKEY, MANIFEST, SIG), Ok(()));
    }

    /// **R18 第 7 条的核心**：签名不对必须拒绝。
    #[test]
    fn rejects_tampered_manifest() {
        let mut bad = MANIFEST.to_vec();
        bad[0] ^= 1; // 改一个比特
        assert_eq!(
            verify_manifest(PUBKEY, &bad, SIG),
            Err(Reject::SignatureMismatch)
        );
    }

    #[test]
    fn rejects_signature_from_another_key() {
        // 换一把公钥去验同一个签名 —— 攻击者拿自己的私钥签了个假清单的情形
        let other = include_str!("testdata/other.pub");
        assert_eq!(
            verify_manifest(other, MANIFEST, SIG),
            Err(Reject::SignatureMismatch)
        );
    }

    #[test]
    fn empty_pubkey_disables_updates_rather_than_skipping_checks() {
        // 绝不能是「没配公钥就不校验」
        assert_eq!(verify_manifest("", MANIFEST, SIG), Err(Reject::NoPubkey));
        assert_eq!(verify_manifest("   ", MANIFEST, SIG), Err(Reject::NoPubkey));
    }

    #[test]
    fn malformed_inputs_are_rejected_not_panicked() {
        assert_eq!(
            verify_manifest("not a key", MANIFEST, SIG),
            Err(Reject::BadPubkey)
        );
        assert_eq!(
            verify_manifest(PUBKEY, MANIFEST, "not a signature"),
            Err(Reject::BadSignature)
        );
        assert_eq!(
            verify_manifest(PUBKEY, MANIFEST, ""),
            Err(Reject::BadSignature)
        );
    }

    #[test]
    fn binary_digest_must_match_the_manifest() {
        // 清单里第一行对应 "hello\n" 的 sha256
        let m =
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03  pulse-agent-test\n";
        assert_eq!(verify_binary(m, "pulse-agent-test", b"hello\n"), Ok(()));
        assert_eq!(
            verify_binary(m, "pulse-agent-test", b"hello!\n"),
            Err(Reject::DigestMismatch)
        );
        assert_eq!(
            verify_binary(m, "pulse-agent-other", b"hello\n"),
            Err(Reject::NotInManifest)
        );
    }

    #[test]
    fn asset_name_matches_the_build_target() {
        let n = asset_name();
        assert!(n.starts_with("pulse-agent-"));
        assert!(n.contains(env!("PULSE_TARGET")));
    }
}
