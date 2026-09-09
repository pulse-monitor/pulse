//! 企业微信群机器人。
//!
//! 两个限制（官方文档）：
//! - 内容上限 **4096 字节**（不是字符）—— 中文一个字 3 字节，很容易超
//! - 频率上限 **20 条/分钟**

use serde::{Deserialize, Serialize};

use super::{NotifyMessage, SEND_TIMEOUT};

/// 企业微信 markdown 的内容上限，字节。
pub const MAX_BYTES: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 群机器人 webhook 的 key
    pub key: String,
}

/// 按**字节**截断，且不切碎 UTF-8 字符。
///
/// 直接 `s[..4096]` 会在多字节字符中间切开，导致整条消息变成乱码或被拒。
fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let marker = "…（已截断）";
    let budget = max.saturating_sub(marker.len());
    let mut end = budget;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{marker}", &s[..end])
}

pub fn build(cfg: &Config, msg: &NotifyMessage) -> (String, serde_json::Value) {
    let color = if msg.resolved { "info" } else { "warning" };
    let icon = if msg.resolved { "✅" } else { "🔴" };
    let content = truncate_bytes(
        &format!(
            "{icon} <font color=\"{color}\">**{}**</font>\n{}",
            msg.title, msg.body
        ),
        MAX_BYTES,
    );
    (
        format!(
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key={}",
            cfg.key
        ),
        serde_json::json!({ "msgtype": "markdown", "markdown": { "content": content } }),
    )
}

pub async fn send(cfg: &Config, msg: &NotifyMessage) -> anyhow::Result<()> {
    let (url, body) = build(cfg, msg);
    let resp = reqwest::Client::builder()
        .timeout(SEND_TIMEOUT)
        .build()?
        .post(&url)
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    // 企业微信即使出错也返回 HTTP 200，真正的结果在 body 的 errcode 里 ——
    // 只看状态码会把「key 无效」当成发送成功
    if !status.is_success() || !text.contains("\"errcode\":0") {
        anyhow::bail!("企业微信返回 {status}: {text}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_carries_the_key() {
        let (url, body) = build(
            &Config {
                key: "abc-123".into(),
            },
            &NotifyMessage::default(),
        );
        assert!(url.ends_with("key=abc-123"));
        assert_eq!(body["msgtype"], "markdown");
    }

    #[test]
    fn long_content_is_truncated_at_a_char_boundary() {
        // 中文一个字 3 字节，很容易超 4096；在字符中间切开会让整条消息变乱码
        let msg = NotifyMessage {
            title: "标题".into(),
            body: "中".repeat(3000), // 9000 字节
            resolved: false,
        };
        let (_, body) = build(&Config { key: "k".into() }, &msg);
        let content = body["markdown"]["content"].as_str().unwrap();
        assert!(content.len() <= MAX_BYTES, "实际 {} 字节", content.len());
        assert!(content.ends_with("（已截断）"), "截断要有明确标记");
        // 能正常解析成 UTF-8 就说明没切碎字符
        assert!(content.chars().all(|c| c != '\u{fffd}'), "不能出现替换字符");
    }

    #[test]
    fn short_content_is_untouched() {
        let (_, body) = build(
            &Config { key: "k".into() },
            &NotifyMessage {
                title: "短".into(),
                body: "正文".into(),
                resolved: false,
            },
        );
        let c = body["markdown"]["content"].as_str().unwrap();
        assert!(c.contains("短") && c.contains("正文"));
        assert!(!c.contains("已截断"));
    }

    #[test]
    fn truncate_never_panics_on_multibyte_boundaries() {
        for n in 0..80 {
            let s = "中".repeat(n);
            for max in 0..40 {
                let _ = truncate_bytes(&s, max); // 不能 panic
            }
        }
    }
}
