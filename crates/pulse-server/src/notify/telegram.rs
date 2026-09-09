//! Telegram Bot。
//!
//! 坑：`parse_mode=HTML` 时正文里的 `< > &` **必须转义**，否则含
//! `<` 的主机名会让 Telegram 直接拒收整条消息（400 Bad Request）。

use serde::{Deserialize, Serialize};

use super::{NotifyMessage, SEND_TIMEOUT};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub token: String,
    pub chat_id: String,
    /// 可选代理。国内服务器直连 api.telegram.org 通常不通
    #[serde(default)]
    pub proxy: Option<String>,
}

/// Telegram 的 HTML 模式只需要转义这三个字符（官方文档明确列出）。
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            _ => out.push(c),
        }
    }
    out
}

/// 构造请求。纯函数 —— 转义与结构都能单测。
pub fn build(cfg: &Config, msg: &NotifyMessage) -> (String, serde_json::Value) {
    let icon = if msg.resolved { "✅" } else { "🔴" };
    let text = format!(
        "{icon} <b>{}</b>\n\n{}",
        escape_html(&msg.title),
        escape_html(&msg.body)
    );
    (
        format!("https://api.telegram.org/bot{}/sendMessage", cfg.token),
        serde_json::json!({
            "chat_id": cfg.chat_id,
            "text": text,
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        }),
    )
}

pub async fn send(cfg: &Config, msg: &NotifyMessage) -> anyhow::Result<()> {
    let (url, body) = build(cfg, msg);
    let mut b = reqwest::Client::builder().timeout(SEND_TIMEOUT);
    if let Some(p) = &cfg.proxy {
        b = b.proxy(reqwest::Proxy::all(p)?);
    }
    let resp = b.build()?.post(&url).json(&body).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        // 带上响应体：Telegram 的错误信息很具体（chat not found / bot blocked…），
        // 吞掉它会让用户完全不知道该改什么
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Telegram 返回 {status}: {text}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            token: "123:ABC".into(),
            chat_id: "-100999".into(),
            proxy: None,
        }
    }

    #[test]
    fn html_special_chars_are_escaped() {
        // 一个含 `<` 的主机名不转义就会让整条消息被 Telegram 拒收
        let msg = NotifyMessage {
            title: "CPU 过高 <node-1>".into(),
            body: "使用率 95% & 持续 5 分钟；阈值 >90%".into(),
            resolved: false,
        };
        let (_, body) = build(&cfg(), &msg);
        let text = body["text"].as_str().unwrap();
        assert!(text.contains("&lt;node-1&gt;"));
        assert!(text.contains("95% &amp; 持续"));
        assert!(text.contains("&gt;90%"));
        // 我们自己的标签要保留
        assert!(text.contains("<b>") && text.contains("</b>"));
    }

    #[test]
    fn url_carries_the_token_and_body_carries_the_chat() {
        let (url, body) = build(&cfg(), &NotifyMessage::default());
        assert_eq!(url, "https://api.telegram.org/bot123:ABC/sendMessage");
        assert_eq!(body["chat_id"], "-100999");
        assert_eq!(body["parse_mode"], "HTML");
    }

    #[test]
    fn resolved_messages_use_a_different_icon() {
        let (_, a) = build(
            &cfg(),
            &NotifyMessage {
                resolved: false,
                ..Default::default()
            },
        );
        let (_, b) = build(
            &cfg(),
            &NotifyMessage {
                resolved: true,
                ..Default::default()
            },
        );
        assert!(a["text"].as_str().unwrap().starts_with("🔴"));
        assert!(b["text"].as_str().unwrap().starts_with("✅"));
    }
}
