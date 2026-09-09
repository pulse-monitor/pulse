//! TCP 与 HTTP 探测。**都不需要任何特权。**

use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// TCP `connect()` 计时。返回微秒；连不上或超时返回 `None`（计为丢包）。
///
/// 这是默认的探测方式：零特权，且几乎所有目标都有至少一个开着的端口。
pub async fn tcp_probe(host: &str, port: u16, timeout: Duration) -> Option<u32> {
    let addr = format!("{host}:{port}");
    let t0 = Instant::now();
    match tokio::time::timeout(timeout, TcpStream::connect(&addr)).await {
        Ok(Ok(stream)) => {
            // 立刻关掉，不占对端资源
            drop(stream);
            Some(elapsed_us(t0))
        }
        // 连接被拒 / DNS 失败 / 超时都算丢包，不是错误 ——
        // 探针不能因为目标不通就崩或刷错误日志
        _ => None,
    }
}

/// HTTP 探测：连接 + 发一个 HEAD + 读状态行。
///
/// 刻意**不引入完整的 HTTP 客户端**：agent 要保持小，而这里只需要
/// 「服务是否在响应、状态码对不对」。`https://` 走已有的 rustls。
pub async fn http_probe(url: &str, expect: Option<u16>, timeout: Duration) -> Option<u32> {
    let (https, host, port, path) = parse_url(url)?;
    let t0 = Instant::now();

    let result = tokio::time::timeout(timeout, async {
        let stream = TcpStream::connect((host.as_str(), port)).await.ok()?;
        let req = format!(
            "HEAD {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: pulse-agent\r\n\
             Connection: close\r\nAccept: */*\r\n\r\n"
        );
        let status = if https {
            let s = tls_connect(stream, &host).await?;
            exchange(s, &req).await?
        } else {
            exchange(stream, &req).await?
        };
        Some(status)
    })
    .await;

    match result {
        Ok(Some(status)) => match expect {
            // 指定了期望状态码就必须相符；没指定则只要有响应就算通
            Some(want) if status != want => None,
            _ => Some(elapsed_us(t0)),
        },
        _ => None,
    }
}

async fn tls_connect(
    stream: TcpStream,
    host: &str,
) -> Option<tokio_rustls::client::TlsStream<TcpStream>> {
    use tokio_rustls::rustls::{ClientConfig, RootCertStore};
    use tokio_rustls::TlsConnector;

    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = host.to_string().try_into().ok()?;
    TlsConnector::from(std::sync::Arc::new(config))
        .connect(name, stream)
        .await
        .ok()
}

/// 发请求并解析状态行。只读首行 —— 我们不关心响应体。
async fn exchange<S>(mut s: S, req: &str) -> Option<u16>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    s.write_all(req.as_bytes()).await.ok()?;
    s.flush().await.ok()?;

    // 状态行不会超过这个长度；读太多是给恶意服务端的放大空间
    let mut buf = [0u8; 256];
    let n = s.read(&mut buf).await.ok()?;
    let head = std::str::from_utf8(&buf[..n]).ok()?;
    // "HTTP/1.1 200 OK"
    head.split_whitespace().nth(1)?.parse().ok()
}

/// 极简 URL 解析：返回 `(是否 https, 主机, 端口, 路径)`。
fn parse_url(url: &str) -> Option<(bool, String, u16, String)> {
    let (https, rest) = match url.strip_prefix("https://") {
        Some(r) => (true, r),
        // 没有 scheme 的一律拒绝：把 "evil.com" 当成相对路径去猜是危险的
        None => (false, url.strip_prefix("http://")?),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return None;
    }
    // 只处理 host[:port]，不支持 userinfo —— 探测目标不该带凭据
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            (h, p.parse().ok()?)
        }
        _ => (authority, if https { 443 } else { 80 }),
    };
    Some((https, host.to_string(), port, path.to_string()))
}

fn elapsed_us(t0: Instant) -> u32 {
    t0.elapsed().as_micros().min(u128::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_parsing() {
        assert_eq!(
            parse_url("https://example.com/health"),
            Some((true, "example.com".into(), 443, "/health".into()))
        );
        assert_eq!(
            parse_url("http://example.com"),
            Some((false, "example.com".into(), 80, "/".into()))
        );
        assert_eq!(
            parse_url("http://127.0.0.1:8080/a/b"),
            Some((false, "127.0.0.1".into(), 8080, "/a/b".into()))
        );
    }

    #[test]
    fn url_parsing_rejects_junk() {
        // 没有 scheme 的一律拒绝，避免把 "evil.com" 当成相对路径去猜
        for bad in ["", "example.com", "ftp://x", "https://", "//x"] {
            assert!(parse_url(bad).is_none(), "不该接受 {bad:?}");
        }
    }

    #[test]
    fn url_parsing_handles_ipv6_literal_without_crashing() {
        // rsplit_once(':') 遇到 IPv6 字面量会切错，但至少不能 panic
        let r = parse_url("http://[::1]:8080/");
        assert!(r.is_some());
    }
}
