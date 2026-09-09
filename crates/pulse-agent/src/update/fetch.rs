//! 更新用的最小 HTTP(S) GET。
//!
//! 刻意**不引入完整 HTTP 客户端**：agent 要保持小，而这里只需要
//! 「把几个固定 URL 的内容下下来」。TLS 复用探测器已有的 rustls 配置。
//!
//! 安全上的关键点不在这一层 —— 下载到什么都要经过 [`super::verify`] 的
//! 签名与摘要校验。所以这里只需要做到：**有大小上限、有超时、不 panic**。

use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// 单次请求的超时。更新不是实时操作，给得宽松些，但必须有上限。
const TIMEOUT: Duration = Duration::from_secs(120);
/// 最多跟随几次重定向。GitHub Releases 会 302 到对象存储，所以必须支持；
/// 但要有上限，否则一个循环重定向就能把 agent 挂在这儿。
const MAX_REDIRECTS: usize = 3;

/// GET 一个 URL，最多读 `max` 字节。
///
/// 超过上限直接报错而不是截断 —— 截断后的内容拿去校验必然失败，
/// 但错误信息会指向「校验不通过」，把真正的原因（超大响应）藏起来。
pub async fn get(url: &str, max: usize) -> Result<Vec<u8>> {
    let mut url = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let (status, headers, body) = tokio::time::timeout(TIMEOUT, get_once(&url, max))
            .await
            .with_context(|| format!("下载 {url} 超时"))??;
        match status {
            200 => return Ok(body),
            301 | 302 | 303 | 307 | 308 => {
                let loc = location(&headers)
                    .with_context(|| format!("{status} 重定向但没有 Location 头"))?;
                url = resolve(&url, loc).with_context(|| format!("无法解析重定向目标 {loc}"))?;
            }
            s => bail!("下载 {url} 失败：HTTP {s}"),
        }
    }
    bail!("重定向次数超过 {MAX_REDIRECTS} 次")
}

async fn get_once(url: &str, max: usize) -> Result<(u16, String, Vec<u8>)> {
    let (https, host, port, path) = parse_url(url).with_context(|| format!("非法 URL: {url}"))?;
    let stream = TcpStream::connect((host.as_str(), port))
        .await
        .with_context(|| format!("连接 {host}:{port} 失败"))?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: pulse-agent\r\n\
         Accept: */*\r\nConnection: close\r\n\r\n"
    );
    let raw = if https {
        let s = tls_connect(stream, &host).await?;
        exchange(s, &req, max).await?
    } else {
        exchange(stream, &req, max).await?
    };
    split_response(&raw, max)
}

async fn tls_connect(
    stream: TcpStream,
    host: &str,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    use tokio_rustls::rustls::{ClientConfig, RootCertStore};
    use tokio_rustls::TlsConnector;

    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = host
        .to_string()
        .try_into()
        .map_err(|_| anyhow::anyhow!("非法主机名 {host}"))?;
    TlsConnector::from(std::sync::Arc::new(config))
        .connect(name, stream)
        .await
        .with_context(|| format!("与 {host} 的 TLS 握手失败"))
}

/// 发请求并读到 EOF（我们发了 `Connection: close`）。
async fn exchange<S>(mut s: S, req: &str, max: usize) -> Result<Vec<u8>>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    s.write_all(req.as_bytes()).await?;
    s.flush().await?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16 << 10];
    loop {
        let n = s.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        // 头 + 体的总量上限。留一点余量给响应头
        if buf.len() > max + (64 << 10) {
            bail!("响应超过 {max} 字节上限");
        }
    }
    Ok(buf)
}

/// 把原始响应切成 `(状态码, 头部原文, 响应体)`，必要时解 chunked。
fn split_response(raw: &[u8], max: usize) -> Result<(u16, String, Vec<u8>)> {
    let sep = find(raw, b"\r\n\r\n").context("响应里找不到头体分隔")?;
    let head = std::str::from_utf8(&raw[..sep]).context("响应头不是有效的 UTF-8")?;
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .context("状态行解析失败")?;
    let headers = head.to_ascii_lowercase();
    let body = &raw[sep + 4..];
    let body = if headers.contains("transfer-encoding: chunked") {
        dechunk(body)?
    } else {
        body.to_vec()
    };
    if body.len() > max {
        bail!("响应体 {} 字节，超过 {max} 上限", body.len());
    }
    Ok((status, head.to_string(), body))
}

/// 解 `Transfer-Encoding: chunked`。
///
/// 格式：`<十六进制长度>\r\n<数据>\r\n` 重复，以长度 0 结束。
/// 任何格式错误都报错，不做「尽力解析」—— 半截数据拿去校验只会得到误导性的错误。
fn dechunk(mut b: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let nl = find(b, b"\r\n").context("chunk 长度行不完整")?;
        let line = std::str::from_utf8(&b[..nl]).context("chunk 长度行不是 UTF-8")?;
        // 长度后面可能跟 ";扩展"，取分号前
        let hex = line.split(';').next().unwrap_or("").trim();
        let n =
            usize::from_str_radix(hex, 16).with_context(|| format!("非法 chunk 长度 {hex:?}"))?;
        b = &b[nl + 2..];
        if n == 0 {
            return Ok(out);
        }
        if b.len() < n + 2 {
            bail!("chunk 数据不完整");
        }
        out.extend_from_slice(&b[..n]);
        b = &b[n + 2..];
    }
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

/// 从响应头里取 `Location`。
fn location(head: &str) -> Option<&str> {
    head.lines()
        .find(|l| l.to_ascii_lowercase().starts_with("location:"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim())
        .filter(|v| !v.is_empty())
}

/// 把重定向目标解析成绝对 URL。
///
/// 只支持绝对 URL 与**同源的**绝对路径。相对路径与 `//host/path` 一律拒绝 ——
/// 它们的解析规则细节多，而更新源本来就不需要用到。
fn resolve(base: &str, loc: &str) -> Option<String> {
    if loc.starts_with("http://") || loc.starts_with("https://") {
        return Some(loc.to_string());
    }
    if let Some(path) = loc.strip_prefix('/') {
        if path.starts_with('/') {
            return None; // 协议相对的 //host/path，不支持
        }
        let (https, host, port, _) = parse_url(base)?;
        let scheme = if https { "https" } else { "http" };
        let default = if https { 443 } else { 80 };
        return Some(if port == default {
            format!("{scheme}://{host}/{path}")
        } else {
            format!("{scheme}://{host}:{port}/{path}")
        });
    }
    None
}

/// 极简 URL 解析：`(是否 https, 主机, 端口, 路径)`。
fn parse_url(url: &str) -> Option<(bool, String, u16, String)> {
    let (https, rest) = match url.strip_prefix("https://") {
        Some(r) => (true, r),
        None => (false, url.strip_prefix("http://")?),
    };
    // 查询串留在 path 里一起发出去
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() || authority.contains('@') {
        return None; // 不支持 userinfo：更新源不该带凭据
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            (h, p.parse().ok()?)
        }
        _ => (authority, if https { 443 } else { 80 }),
    };
    Some((https, host.to_string(), port, path.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_plain_response() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        let (s, _, b) = split_response(raw, 1024).unwrap();
        assert_eq!(s, 200);
        assert_eq!(b, b"hello");
    }

    #[test]
    fn dechunks() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                    5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let (_, _, b) = split_response(raw, 1024).unwrap();
        assert_eq!(b, b"hello world");
    }

    #[test]
    fn dechunk_handles_extensions_and_rejects_garbage() {
        assert_eq!(dechunk(b"3;foo=bar\r\nabc\r\n0\r\n\r\n").unwrap(), b"abc");
        assert!(dechunk(b"zz\r\nabc\r\n").is_err());
        assert!(
            dechunk(b"5\r\nab\r\n").is_err(),
            "数据不足必须报错而不是返回半截"
        );
        assert!(dechunk(b"").is_err());
    }

    #[test]
    fn oversize_body_errors_instead_of_truncating() {
        // 截断后的内容拿去校验必然失败，但错误会指向「校验不通过」，
        // 把真正的原因藏起来
        let raw = b"HTTP/1.1 200 OK\r\n\r\n0123456789";
        let e = split_response(raw, 5).unwrap_err().to_string();
        assert!(e.contains("上限"), "错误应当说明是超限: {e}");
    }

    #[test]
    fn finds_location_case_insensitively() {
        assert_eq!(
            location("HTTP/1.1 302 Found\r\nLocation: https://x/y\r\n"),
            Some("https://x/y")
        );
        assert_eq!(
            location("HTTP/1.1 302 Found\r\nlocation:  https://x/y  \r\n"),
            Some("https://x/y")
        );
        assert_eq!(location("HTTP/1.1 302 Found\r\n"), None);
        assert_eq!(location("HTTP/1.1 302 Found\r\nLocation:\r\n"), None);
    }

    #[test]
    fn resolves_redirect_targets() {
        assert_eq!(
            resolve("https://a.com/x", "https://b.com/y"),
            Some("https://b.com/y".into())
        );
        assert_eq!(
            resolve("https://a.com/x", "/y/z"),
            Some("https://a.com/y/z".into())
        );
        assert_eq!(
            resolve("http://a.com:8080/x", "/y"),
            Some("http://a.com:8080/y".into())
        );
        // 协议相对与相对路径都不支持
        assert_eq!(resolve("https://a.com/x", "//evil.com/y"), None);
        assert_eq!(resolve("https://a.com/x", "y"), None);
    }

    #[test]
    fn url_parsing_rejects_credentials_and_junk() {
        assert!(
            parse_url("https://user:pw@host/x").is_none(),
            "不该接受 userinfo"
        );
        for bad in ["", "host/x", "ftp://x", "https://"] {
            assert!(parse_url(bad).is_none(), "不该接受 {bad:?}");
        }
    }
}
