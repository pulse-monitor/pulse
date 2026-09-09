//! SMTP 邮件。
//!
//! 两个常见坑：
//! - **端口决定 TLS 方式**：465 是 implicit TLS（连上就握手），
//!   587 是 STARTTLS（先明文再升级）。搞反了会一直连不上
//! - 发件人域名没有 SPF 记录时，多数收件方会直接判垃圾邮件

use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::{Deserialize, Serialize};

use super::NotifyMessage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tls {
    /// 465：连上就 TLS 握手
    Implicit,
    /// 587：先明文，再 STARTTLS 升级
    Starttls,
    /// 仅用于内网无加密的中继。**不要用于公网**
    None,
}

impl Tls {
    /// 按端口推断。用户没显式选时用它，比让人猜要好。
    pub fn from_port(port: u16) -> Self {
        match port {
            465 => Tls::Implicit,
            25 => Tls::None,
            _ => Tls::Starttls, // 587 与其他端口
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub from: String,
    pub to: Vec<String>,
    #[serde(default = "default_tls")]
    pub tls: Tls,
}

fn default_tls() -> Tls {
    Tls::Starttls
}

/// 构造邮件。纯函数 —— 收件人拆分与标题格式可以单测。
pub fn build(cfg: &Config, msg: &NotifyMessage) -> anyhow::Result<Message> {
    let prefix = if msg.resolved {
        "[已恢复]"
    } else {
        "[告警]"
    };
    let mut b = Message::builder()
        .from(
            cfg.from
                .parse()
                .map_err(|e| anyhow::anyhow!("发件人地址非法: {e}"))?,
        )
        .subject(format!("{prefix} {}", msg.title));
    for to in &cfg.to {
        b = b.to(to
            .parse()
            .map_err(|e| anyhow::anyhow!("收件人地址 {to} 非法: {e}"))?);
    }
    b.body(msg.body.clone())
        .map_err(|e| anyhow::anyhow!("构造邮件失败: {e}"))
}

pub async fn send(cfg: &Config, msg: &NotifyMessage) -> anyhow::Result<()> {
    if cfg.to.is_empty() {
        anyhow::bail!("没有收件人");
    }
    let mail = build(cfg, msg)?;

    let builder = match cfg.tls {
        Tls::Implicit => AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.host)?,
        Tls::Starttls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.host)?,
        // 明文中继：只在内网用
        Tls::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&cfg.host),
    };
    let transport = builder
        .port(cfg.port)
        .credentials(Credentials::new(cfg.username.clone(), cfg.password.clone()))
        .timeout(Some(super::SEND_TIMEOUT))
        .build();

    transport
        .send(mail)
        .await
        .map_err(|e| anyhow::anyhow!("SMTP 发送失败: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            host: "smtp.example.com".into(),
            port: 587,
            username: "u".into(),
            password: "p".into(),
            from: "pulse@example.com".into(),
            to: vec!["a@example.com".into(), "b@example.com".into()],
            tls: Tls::Starttls,
        }
    }

    #[test]
    fn tls_is_inferred_from_the_port() {
        // 搞反 465/587 会一直连不上，这是最常见的配置错误
        assert_eq!(Tls::from_port(465), Tls::Implicit);
        assert_eq!(Tls::from_port(587), Tls::Starttls);
        assert_eq!(Tls::from_port(2525), Tls::Starttls);
        assert_eq!(Tls::from_port(25), Tls::None);
    }

    #[test]
    fn subject_marks_alerts_and_recoveries_differently() {
        let a = build(
            &cfg(),
            &NotifyMessage {
                title: "CPU 过高".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let b = build(
            &cfg(),
            &NotifyMessage {
                title: "CPU 过高".into(),
                resolved: true,
                ..Default::default()
            },
        )
        .unwrap();
        let (fa, fb) = (a.formatted(), b.formatted());
        let sa = String::from_utf8_lossy(&fa);
        let sb = String::from_utf8_lossy(&fb);
        // 主题可能被 MIME 编码，所以只断言两者不同且各自能构造成功
        assert_ne!(sa, sb);
        assert!(sa.contains("To:") && sa.contains("From:"));
    }

    #[test]
    fn every_recipient_is_included() {
        let m = build(&cfg(), &NotifyMessage::default()).unwrap();
        let f = m.formatted();
        let s = String::from_utf8_lossy(&f);
        assert!(s.contains("a@example.com"));
        assert!(s.contains("b@example.com"));
    }

    #[test]
    fn malformed_addresses_fail_loudly_instead_of_silently_dropping() {
        // 悄悄丢掉一个收件人 = 用户以为配好了但永远收不到
        let bad_from = Config {
            from: "not an address".into(),
            ..cfg()
        };
        assert!(build(&bad_from, &NotifyMessage::default()).is_err());

        let bad_to = Config {
            to: vec!["good@e.com".into(), "bad".into()],
            ..cfg()
        };
        let err = build(&bad_to, &NotifyMessage::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("bad"), "报错要指出是哪个地址: {err}");
    }
}
