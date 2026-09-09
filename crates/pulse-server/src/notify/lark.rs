//! 飞书自定义机器人。
//!
//! 开启「签名校验」后必须带 `timestamp` 与 `sign`：
//! `sign = base64(HMAC-SHA256(key = "{timestamp}\n{secret}", data = ""))`
//!
//! 注意这个签名的算法很反直觉 —— **secret 是拼进 key 里，被签的数据是空的**。
//! 写反了会一直收到 19021 错误。

use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use super::{NotifyMessage, SEND_TIMEOUT};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 完整的 webhook URL
    pub webhook: String,
    /// 开启签名校验时填
    #[serde(default)]
    pub secret: Option<String>,
}

/// 计算飞书签名。
pub fn sign(timestamp: i64, secret: &str) -> String {
    let key = format!("{timestamp}\n{secret}");
    let mut mac = <Hmac<Sha256>>::new_from_slice(key.as_bytes()).expect("HMAC 接受任意长度的密钥");
    // 被签的数据是**空**的 —— secret 已经在 key 里了
    mac.update(b"");
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

pub fn build(cfg: &Config, msg: &NotifyMessage, now: i64) -> (String, serde_json::Value) {
    let color = if msg.resolved { "green" } else { "red" };
    let icon = if msg.resolved { "✅" } else { "🔴" };

    let mut body = serde_json::json!({
        "msg_type": "interactive",
        "card": {
            "config": { "wide_screen_mode": true },
            "header": {
                "template": color,
                "title": { "tag": "plain_text", "content": format!("{icon} {}", msg.title) }
            },
            "elements": [
                { "tag": "div", "text": { "tag": "lark_md", "content": msg.body } }
            ]
        }
    });
    if let Some(secret) = &cfg.secret {
        body["timestamp"] = serde_json::json!(now.to_string());
        body["sign"] = serde_json::json!(sign(now, secret));
    }
    (cfg.webhook.clone(), body)
}

pub async fn send(cfg: &Config, msg: &NotifyMessage) -> anyhow::Result<()> {
    let (url, body) = build(cfg, msg, crate::state::now_unix());
    let resp = reqwest::Client::builder()
        .timeout(SEND_TIMEOUT)
        .build()?
        .post(&url)
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    // 飞书同样是 HTTP 200 + body 里带错误码
    if !status.is_success() || text.contains("\"code\":19") {
        anyhow::bail!("飞书返回 {status}: {text}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_is_stable_and_depends_on_both_inputs() {
        let a = sign(1_700_000_000, "secret");
        assert_eq!(a, sign(1_700_000_000, "secret"), "同样输入必须同样输出");
        assert_ne!(a, sign(1_700_000_001, "secret"), "时间戳参与签名");
        assert_ne!(a, sign(1_700_000_000, "other"), "密钥参与签名");
        // base64 of 32 bytes = 44 chars with padding
        assert_eq!(a.len(), 44, "HMAC-SHA256 的 base64 应当是 44 字符");
    }

    #[test]
    fn signature_uses_secret_as_key_not_as_data() {
        // 这是飞书签名最容易写反的地方。
        // 正确：key = "{ts}\n{secret}"，data = ""
        // 写反的话下面这个手算值对不上
        let expect = {
            let mut mac = <Hmac<Sha256>>::new_from_slice(b"1700000000\nsecret").unwrap();
            mac.update(b"");
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
        };
        assert_eq!(sign(1_700_000_000, "secret"), expect);
    }

    #[test]
    fn signature_fields_appear_only_when_secret_is_set() {
        let msg = NotifyMessage {
            title: "t".into(),
            body: "b".into(),
            resolved: false,
        };
        let no_sig = Config {
            webhook: "https://x".into(),
            secret: None,
        };
        let (_, b) = build(&no_sig, &msg, 100);
        assert!(b.get("sign").is_none(), "没开签名校验时不该带 sign");
        assert!(b.get("timestamp").is_none());

        let with_sig = Config {
            secret: Some("s".into()),
            ..no_sig
        };
        let (_, b) = build(&with_sig, &msg, 100);
        assert_eq!(b["timestamp"], "100", "timestamp 必须是字符串，飞书要求");
        assert_eq!(b["sign"], sign(100, "s"));
    }

    #[test]
    fn card_colour_reflects_resolution() {
        let cfg = Config {
            webhook: "https://x".into(),
            secret: None,
        };
        let (_, a) = build(
            &cfg,
            &NotifyMessage {
                resolved: false,
                ..Default::default()
            },
            0,
        );
        let (_, b) = build(
            &cfg,
            &NotifyMessage {
                resolved: true,
                ..Default::default()
            },
            0,
        );
        assert_eq!(a["card"]["header"]["template"], "red");
        assert_eq!(b["card"]["header"]["template"], "green");
    }
}
