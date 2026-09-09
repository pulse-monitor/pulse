//! 通知渠道（R7）。
//!
//! 四个渠道：邮件 / Telegram / 企业微信 / 飞书。
//!
//! 设计上把**请求构造**与**实际发送**分开：前者是纯函数，
//! 签名、转义、长度限制这些最容易出错的地方因此可以完整单测；
//! 后者只是一次 HTTP/SMTP 调用，需要真凭据才能验。

pub mod crypto;
pub mod email;
pub mod lark;
pub mod telegram;
pub mod template;
pub mod wecom;

use serde::{Deserialize, Serialize};

/// 渠道类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Email,
    Telegram,
    Wecom,
    Lark,
}

impl ChannelKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "email" => ChannelKind::Email,
            "telegram" => ChannelKind::Telegram,
            "wecom" => ChannelKind::Wecom,
            "lark" => ChannelKind::Lark,
            _ => return None,
        })
    }
    pub const fn as_str(self) -> &'static str {
        match self {
            ChannelKind::Email => "email",
            ChannelKind::Telegram => "telegram",
            ChannelKind::Wecom => "wecom",
            ChannelKind::Lark => "lark",
        }
    }
}

/// 各渠道的配置。**含凭据**，落库前必须经 [`crypto::encrypt`]。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChannelConfig {
    Email(email::Config),
    Telegram(telegram::Config),
    Wecom(wecom::Config),
    Lark(lark::Config),
}

impl ChannelConfig {
    pub fn kind(&self) -> ChannelKind {
        match self {
            ChannelConfig::Email(_) => ChannelKind::Email,
            ChannelConfig::Telegram(_) => ChannelKind::Telegram,
            ChannelConfig::Wecom(_) => ChannelKind::Wecom,
            ChannelConfig::Lark(_) => ChannelKind::Lark,
        }
    }

    /// 回显给前端时用。**绝不把明文凭据发回浏览器。**
    pub fn masked(&self) -> serde_json::Value {
        match self {
            ChannelConfig::Email(c) => serde_json::json!({
                "kind": "email", "host": c.host, "port": c.port, "username": c.username,
                "password": crypto::mask(&c.password), "from": c.from, "to": c.to,
                "tls": c.tls,
            }),
            ChannelConfig::Telegram(c) => serde_json::json!({
                "kind": "telegram", "token": crypto::mask(&c.token),
                "chat_id": c.chat_id, "proxy": c.proxy,
            }),
            ChannelConfig::Wecom(c) => serde_json::json!({
                "kind": "wecom", "key": crypto::mask(&c.key),
            }),
            ChannelConfig::Lark(c) => serde_json::json!({
                "kind": "lark", "webhook": crypto::mask(&c.webhook),
                "secret": c.secret.as_deref().map(crypto::mask),
            }),
        }
    }
}

/// 一条待发送的通知。
#[derive(Debug, Clone, Default)]
pub struct NotifyMessage {
    /// 标题/主题
    pub title: String,
    /// 正文（纯文本；各渠道自己转成对应格式）
    pub body: String,
    /// 是否是「已恢复」通知 —— 渠道据此换个图标/颜色
    pub resolved: bool,
}

/// webhook 地址是否可接受。
///
/// **公网必须 https**：明文发送会把签名与消息内容一起暴露在链路上。
/// 但**回环与私有网段允许 http** —— 内网中继、本地调试都是合法用法，
/// 一刀切成 https 会把它们也挡掉。
pub fn is_acceptable_webhook(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false; // 别的 scheme 一律拒绝
    };
    is_private_host(extract_host(rest))
}

/// 从 `host[:port]/path` 里取出主机名。
///
/// IPv6 字面量用方括号包裹（`[::1]:8080`），直接按 `:` 切分会把它切碎 ——
/// 必须先认出方括号。这个坑在延迟探测的 URL 解析里也踩过。
fn extract_host(authority_and_path: &str) -> &str {
    let s = authority_and_path;
    if let Some(rest) = s.strip_prefix('[') {
        // [::1]:8080/x → ::1
        return rest.split(']').next().unwrap_or("");
    }
    s.split(['/', ':', '?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('.')
}

/// 回环 / RFC1918 私有网段 / .local。
fn is_private_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".local") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        // 手写 fc00::/7 判定：Ipv6Addr::is_unique_local 要 Rust 1.84，
        // 而本项目的 MSRV 是 1.82
        Ok(std::net::IpAddr::V6(v6)) => v6.is_loopback() || (v6.octets()[0] & 0xfe) == 0xfc,
        Err(_) => false,
    }
}

/// 发送超时。渠道挂掉时不能拖住整个求值循环。
pub const SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// 重试次数（不含首次）。带退避。
pub const MAX_RETRIES: u32 = 2;

/// 统一的发送入口。
///
/// 失败**只记日志不向上传播** —— 一个渠道配错了不该让其他渠道也发不出去，
/// 更不该让整个告警求值循环停掉。
pub async fn send(cfg: &ChannelConfig, msg: &NotifyMessage) -> anyhow::Result<()> {
    let mut last_err = None;
    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            // 指数退避：1s、2s
            tokio::time::sleep(std::time::Duration::from_secs(1 << (attempt - 1))).await;
        }
        let r = match cfg {
            ChannelConfig::Email(c) => email::send(c, msg).await,
            ChannelConfig::Telegram(c) => telegram::send(c, msg).await,
            ChannelConfig::Wecom(c) => wecom::send(c, msg).await,
            ChannelConfig::Lark(c) => lark::send(c, msg).await,
        };
        match r {
            Ok(()) => return Ok(()),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("发送失败")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_webhooks_must_be_https_but_private_ones_need_not_be() {
        // 明文发到公网会把签名与消息内容一起暴露；发到内网则无所谓
        assert!(is_acceptable_webhook(
            "https://open.feishu.cn/open-apis/bot/v2/hook/x"
        ));
        assert!(!is_acceptable_webhook(
            "http://open.feishu.cn/open-apis/bot/v2/hook/x"
        ));

        // 回环与私有网段允许 http —— 内网中继、本地调试都是合法用法
        for ok in [
            "http://127.0.0.1:25899/hook",
            "http://localhost:8080/x",
            "http://10.0.0.5/hook",
            "http://192.168.1.10:9000/hook",
            "http://172.16.0.1/hook",
            "http://relay.local/hook",
            "http://[::1]:8080/x",
        ] {
            assert!(is_acceptable_webhook(ok), "应当接受 {ok}");
        }

        // 别的 scheme 与畸形输入一律拒绝
        for bad in [
            "",
            "ftp://x",
            "//x",
            "javascript:alert(1)",
            "http://8.8.8.8/hook",
        ] {
            assert!(!is_acceptable_webhook(bad), "不该接受 {bad:?}");
        }
    }

    #[test]
    fn channel_kind_roundtrip() {
        for k in [
            ChannelKind::Email,
            ChannelKind::Telegram,
            ChannelKind::Wecom,
            ChannelKind::Lark,
        ] {
            assert_eq!(ChannelKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(ChannelKind::parse("sms"), None);
    }

    #[test]
    fn masked_config_never_leaks_credentials() {
        // 用户改配置时只需要确认「填过了」，不需要看到内容
        let cfgs = [
            ChannelConfig::Telegram(telegram::Config {
                token: "1234567890:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw".into(),
                chat_id: "-100123".into(),
                proxy: None,
            }),
            ChannelConfig::Wecom(wecom::Config {
                key: "abcdef-1234-5678-secret".into(),
            }),
            ChannelConfig::Email(email::Config {
                host: "smtp.example.com".into(),
                port: 587,
                username: "u".into(),
                password: "SuperSecretPassword".into(),
                from: "a@b.c".into(),
                to: vec!["d@e.f".into()],
                tls: email::Tls::Starttls,
            }),
        ];
        for c in &cfgs {
            let s = serde_json::to_string(&c.masked()).unwrap();
            for leak in [
                "AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw",
                "abcdef-1234-5678-secret",
                "SuperSecretPassword",
            ] {
                assert!(!s.contains(leak), "掩码后仍然泄露了凭据: {s}");
            }
        }
    }

    #[test]
    fn config_roundtrips_through_json() {
        // 配置以 JSON 加密存库，必须能原样读回
        let c = ChannelConfig::Lark(lark::Config {
            webhook: "https://open.feishu.cn/open-apis/bot/v2/hook/xxx".into(),
            secret: Some("sign-secret".into()),
        });
        let s = serde_json::to_string(&c).unwrap();
        let back: ChannelConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back.kind(), ChannelKind::Lark);
    }
}
