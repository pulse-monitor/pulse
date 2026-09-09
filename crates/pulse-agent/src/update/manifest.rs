//! 更新清单的解析与版本比较。**纯逻辑，不碰网络也不碰文件系统。**
//!
//! 清单就是 `sha256sum` 的标准输出格式，这样发布流程里能直接用系统自带的
//! `sha256sum` / `shasum -a 256` 生成，不需要额外工具：
//!
//! ```text
//! 3b1f…  pulse-agent-x86_64-unknown-linux-musl
//! a90c…  pulse-agent-aarch64-unknown-linux-musl
//! ```

/// 从清单里取出某个文件名对应的 SHA-256。
///
/// 找不到返回 `None` —— **绝不能**回落成「不校验」。
pub fn sha256_of(manifest: &str, filename: &str) -> Option<[u8; 32]> {
    for line in manifest.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // "<hex>  <name>" 或 "<hex> *<name>"（二进制模式）
        let (hex, name) = line.split_once(char::is_whitespace)?;
        let name = name.trim_start().trim_start_matches('*');
        if name != filename {
            continue;
        }
        return decode_hex32(hex);
    }
    None
}

/// 64 个十六进制字符 → 32 字节。任何长度或字符不对都返回 `None`。
fn decode_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let b = s.as_bytes();
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        let hi = (b[i * 2] as char).to_digit(16)?;
        let lo = (b[i * 2 + 1] as char).to_digit(16)?;
        *o = (hi * 16 + lo) as u8;
    }
    Some(out)
}

/// 语义化版本号。只认 `major.minor.patch`，多余的后缀（`-rc1`、`+build`）一律拒绝
/// —— 自动更新的判定必须是全序的，预发布版本的排序规则会引入歧义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u32, u32, u32);

impl Version {
    pub fn parse(s: &str) -> Option<Self> {
        let mut it = s.trim().split('.');
        let a = it.next()?.parse().ok()?;
        let b = it.next()?.parse().ok()?;
        let c = it.next()?.parse().ok()?;
        if it.next().is_some() {
            return None;
        }
        Some(Version(a, b, c))
    }
}

/// 能不能升到 `candidate`。
///
/// **第 4 道防线（版本单调）**：只接受严格更新的版本。
/// 面板被攻陷后想把 agent 降级到有已知漏洞的旧版本 —— 这里挡住。
/// 版本号解析失败也一律拒绝，不做「看不懂就升」的乐观处理。
pub fn is_upgrade(current: &str, candidate: &str) -> bool {
    match (Version::parse(current), Version::parse(candidate)) {
        (Some(cur), Some(new)) => new > cur,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: &str = "\
3b1f2c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f809  pulse-agent-x86_64-unknown-linux-musl
a90cbbccddeeff00112233445566778899aabbccddeeff00112233445566778899  too-long-line-ignored
0000000000000000000000000000000000000000000000000000000000000001 *pulse-agent-x86_64-pc-windows-msvc.exe
";

    #[test]
    fn picks_the_right_line() {
        let h = sha256_of(M, "pulse-agent-x86_64-unknown-linux-musl").unwrap();
        assert_eq!(h[0], 0x3b);
        assert_eq!(h[31], 0x09);
    }

    #[test]
    fn supports_binary_mode_star_prefix() {
        let h = sha256_of(M, "pulse-agent-x86_64-pc-windows-msvc.exe").unwrap();
        assert_eq!(h[31], 1);
    }

    #[test]
    fn unknown_file_is_none_never_a_default() {
        // 关键：查不到必须是 None，绝不能回落成「不校验」
        assert!(sha256_of(M, "pulse-agent-aarch64-unknown-linux-musl").is_none());
        assert!(sha256_of("", "anything").is_none());
    }

    #[test]
    fn malformed_hash_is_rejected() {
        for bad in [
            "zzzz  f",                                                               // 非十六进制
            "3b1f  f",                                                               // 太短
            "3b1f2c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f80900  f", // 太长
        ] {
            assert!(sha256_of(bad, "f").is_none(), "不该接受 {bad:?}");
        }
    }

    #[test]
    fn version_ordering() {
        assert!(is_upgrade("0.0.1", "0.0.2"));
        assert!(is_upgrade("0.9.9", "1.0.0"));
        assert!(is_upgrade("1.2.3", "1.10.0"), "10 > 2，不能按字符串比");
    }

    #[test]
    fn refuses_downgrade_and_sidegrade() {
        // 第 4 道防线：面板被攻陷也不能把 agent 降级到有已知漏洞的旧版
        assert!(!is_upgrade("1.0.0", "0.9.9"));
        assert!(!is_upgrade("1.0.0", "1.0.0"), "同版本不算升级");
    }

    #[test]
    fn unparseable_versions_refuse_rather_than_guess() {
        for (cur, new) in [
            ("1.0.0", "1.0"),       // 位数不够
            ("1.0.0", "1.0.0.1"),   // 位数太多
            ("1.0.0", "1.0.1-rc1"), // 预发布后缀：排序有歧义，不接受
            ("", "1.0.0"),
            ("1.0.0", ""),
            ("1.0.0", "v1.0.1"), // 带 v 前缀
        ] {
            assert!(!is_upgrade(cur, new), "{cur} → {new} 不该被接受");
        }
    }
}
