//! 面板自己终止 TLS。
//!
//! 为什么内置而不是让用户架 nginx / Caddy：这个项目的立项理由就是
//! 「komari、哪吒都太重」，如果部署面板还得先装一层反代，等于把省下来的
//! 复杂度又加回去。配两个文件路径就能跑 HTTPS。
//!
//! **这不是为了取代反代**。已经有 nginx/Caddy 的照旧用，把
//! `PULSE_TRUSTED_PROXY_HOPS` 配上就行（否则拿到的客户端 IP 是反代的）。
//!
//! 为什么这件事重要：探针的 token 是**放在 WebSocket 握手里**发过去的。
//! 明文 ws:// 意味着链路上任何一跳都能拿到 token，拿到就能冒充这台机器上报。

use anyhow::{Context, Result};
use axum_server::tls_rustls::RustlsConfig;
use std::path::Path;

/// 证书与私钥的路径。两个都给了才启用 HTTPS。
pub struct TlsPaths {
    pub cert: String,
    pub key: String,
}

impl TlsPaths {
    /// 从环境变量读。只配了一个就是配置错误，直接报错而不是悄悄降级成 HTTP ——
    /// 「以为开了 TLS 其实没开」比「起不来」危险得多。
    pub fn from_env(cert: Option<String>, key: Option<String>) -> Result<Option<Self>> {
        match (cert, key) {
            (Some(c), Some(k)) if !c.is_empty() && !k.is_empty() => {
                Ok(Some(TlsPaths { cert: c, key: k }))
            }
            (Some(c), None) | (Some(c), Some(_)) if !c.is_empty() => {
                anyhow::bail!("配了 PULSE_TLS_CERT（{c}）却没配 PULSE_TLS_KEY，两个都要给")
            }
            (None, Some(k)) if !k.is_empty() => {
                anyhow::bail!("配了 PULSE_TLS_KEY（{k}）却没配 PULSE_TLS_CERT，两个都要给")
            }
            _ => Ok(None),
        }
    }

    /// 加载证书。报错时把路径带上 —— 「文件不存在」不说是哪个文件等于没说。
    pub async fn load(&self) -> Result<RustlsConfig> {
        for (what, p) in [("证书", &self.cert), ("私钥", &self.key)] {
            let path = Path::new(p);
            if !path.exists() {
                anyhow::bail!("{what}文件不存在：{p}");
            }
            // 私钥被别人读到就等于证书作废，这里只是提醒，不强制
            #[cfg(unix)]
            if what == "私钥" {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(m) = std::fs::metadata(path) {
                    let mode = m.permissions().mode() & 0o077;
                    if mode != 0 {
                        tracing::warn!(
                            path = %p,
                            mode = format!("{:o}", m.permissions().mode() & 0o777),
                            "私钥对同组或其他用户可读，建议 chmod 600"
                        );
                    }
                }
            }
        }
        RustlsConfig::from_pem_file(&self.cert, &self.key)
            .await
            .with_context(|| format!("加载 TLS 证书失败（cert={} key={}）", self.cert, self.key))
    }
}

/// 面板地址是不是明文 HTTP 发到公网上去了。
///
/// 只看 `panel_url` 而不是「有没有开 TLS」：面板可能自己跑 HTTP、
/// 前面由反代终止 HTTPS，那种情况下 `panel_url` 是 https，一切正常。
/// 真正的问题是**装机命令里发出去的地址**是明文的。
pub fn insecure_public_url(panel_url: &str) -> bool {
    let Some(rest) = panel_url.strip_prefix("http://") else {
        return false;
    };
    // IPv6 要先摘方括号再切端口 —— 直接按 ':' 切会把 `[::1]:25774` 切成 `[`
    let authority = rest.split('/').next().unwrap_or("");
    let host = if let Some(inner) = authority.strip_prefix('[') {
        inner.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    !is_local(host)
}

/// 本机地址。本机上明文 HTTP 没有链路可窃听，不必告警。
fn is_local(host: &str) -> bool {
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        Ok(std::net::IpAddr::V6(v6)) => {
            // is_unique_local / is_unicast_link_local 还没稳定，自己按前缀判
            v6.is_loopback() || {
                let s = v6.segments();
                (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80
            }
        }
        // 0.0.0.0 这种绑定地址进不到这里（不是合法主机名也不是可达地址），
        // 但域名会 —— 域名一律当作公网
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 只配一半就报错() {
        assert!(TlsPaths::from_env(Some("c.pem".into()), None).is_err());
        assert!(TlsPaths::from_env(None, Some("k.pem".into())).is_err());
        assert!(TlsPaths::from_env(None, None).unwrap().is_none());
        assert!(TlsPaths::from_env(Some("c".into()), Some("k".into()))
            .unwrap()
            .is_some());
    }

    #[test]
    fn 空字符串等同没配() {
        // systemd 的 EnvironmentFile 里写 `PULSE_TLS_CERT=` 会得到空串，
        // 那是「没配」而不是「配了个空路径」
        assert!(TlsPaths::from_env(Some(String::new()), Some(String::new()))
            .unwrap()
            .is_none());
    }

    #[test]
    fn 公网明文地址会被识别出来() {
        assert!(insecure_public_url("http://31.22.111.30:25774"));
        assert!(insecure_public_url("http://panel.example.com"));
        assert!(insecure_public_url("http://panel.example.com:8080/"));
    }

    #[test]
    fn https_和本机地址不告警() {
        assert!(!insecure_public_url("https://panel.example.com"));
        assert!(!insecure_public_url("http://localhost:25774"));
        assert!(!insecure_public_url("http://127.0.0.1:25774"));
        assert!(!insecure_public_url("http://[::1]:25774"));
        // 内网地址：链路在自己家里
        assert!(!insecure_public_url("http://192.168.1.10:25774"));
        assert!(!insecure_public_url("http://10.0.0.5:25774"));
        assert!(!insecure_public_url("http://172.16.0.1:25774"));
        assert!(!insecure_public_url("http://[fd00::1]:25774"));
    }

    #[test]
    fn 公网_ipv6_会告警() {
        assert!(insecure_public_url("http://[2001:db8::1]:25774"));
    }
}
