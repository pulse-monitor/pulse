//! 访客悬浮标签（R14）。
//!
//! **隐私约束**：
//! - 访客信息**不落盘、不写访问日志**，只在请求内计算并返回
//! - 站点设置可整体关闭
//! - 字段缺失时不显示那一行，而不是显示「未知」

use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};

use super::{client_ip, ApiError, ApiResult, Ctx};
use crate::state::now_unix;

pub fn routes() -> Router<Ctx> {
    Router::new().route("/api/v1/public/visitor", get(visitor))
}

/// 从 User-Agent 猜操作系统。
///
/// 刻意只认常见的几种：一个猜错的冷门系统名，不如干脆不显示那一行。
/// 顺序有讲究 —— 很多 UA 会同时包含多个关键词。
pub fn parse_os(ua: &str) -> Option<&'static str> {
    // iPadOS 的 UA 里同时有 "Macintosh"，必须先判平板/手机
    if ua.contains("iPhone") {
        return Some("iOS");
    }
    if ua.contains("iPad") {
        return Some("iPadOS");
    }
    if ua.contains("Android") {
        return Some("Android");
    }
    // "Windows NT" 才是桌面 Windows；"Windows Phone" 是另一回事
    if ua.contains("Windows NT") {
        return Some("Windows");
    }
    if ua.contains("Mac OS X") || ua.contains("Macintosh") {
        return Some("macOS");
    }
    // CrOS 里也含 "Linux"，必须先判
    if ua.contains("CrOS") {
        return Some("ChromeOS");
    }
    if ua.contains("Linux") {
        return Some("Linux");
    }
    None
}

/// 从 User-Agent 猜浏览器。
///
/// 顺序极其重要：Edge 的 UA 含 "Chrome"，Chrome 的含 "Safari"，
/// 顺序反了就全都识别成 Safari。
pub fn parse_browser(ua: &str) -> Option<&'static str> {
    for (needle, name) in [
        ("Edg/", "Edge"),  // Edge 含 Chrome
        ("OPR/", "Opera"), // Opera 含 Chrome
        ("Vivaldi", "Vivaldi"),
        ("Firefox/", "Firefox"),
        ("Chrome/", "Chrome"), // Chrome 含 Safari
        ("Safari/", "Safari"),
    ] {
        if ua.contains(needle) {
            return Some(name);
        }
    }
    None
}

async fn visitor(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
) -> ApiResult<impl IntoResponse> {
    if !ctx.config.visitor_badge {
        // 关掉时返回 404 而不是空对象 —— 前端据此完全不渲染
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "DISABLED",
            "访客标签已关闭",
        ));
    }

    let ip = client_ip(&headers, peer.ip(), ctx.config.trusted_proxy_hops);
    let ua = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // 这里**不写任何日志、不落任何盘**。
    // GeoIP（国家 / ASN / IDC）在 M3 按范围决策推迟了，所以是 null，
    // 前端会隐藏对应行而不是显示「未知」。
    Ok(Json(serde_json::json!({
        "ip": ip.to_string(),
        "os": parse_os(ua),
        "browser": parse_browser(ua),
        "time": now_unix(),
        "timezone": ctx.config.timezone.to_string(),
        "country": serde_json::Value::Null,
        "country_name": serde_json::Value::Null,
        "city": serde_json::Value::Null,
        "asn": serde_json::Value::Null,
        "isp": serde_json::Value::Null,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 全部取自真实浏览器
    const CHROME_WIN: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
    const EDGE_WIN: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0";
    const SAFARI_MAC: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";
    const FIREFOX_LINUX: &str =
        "Mozilla/5.0 (X11; Linux x86_64; rv:133.0) Gecko/20100101 Firefox/133.0";
    const SAFARI_IOS: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Mobile/15E148 Safari/604.1";
    const IPAD: &str = "Mozilla/5.0 (iPad; CPU OS 18_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/604.1";
    const CHROME_ANDROID: &str = "Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36";
    const OPERA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36 OPR/111.0.0.0";
    const CHROMEOS: &str = "Mozilla/5.0 (X11; CrOS x86_64 14541.0.0) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

    #[test]
    fn browser_detection_respects_ua_nesting() {
        // 这是 UA 解析最经典的坑：Edge 含 Chrome、Chrome 含 Safari。
        // 顺序反了就全识别成 Safari
        assert_eq!(parse_browser(EDGE_WIN), Some("Edge"));
        assert_eq!(parse_browser(OPERA), Some("Opera"));
        assert_eq!(parse_browser(CHROME_WIN), Some("Chrome"));
        assert_eq!(parse_browser(SAFARI_MAC), Some("Safari"));
        assert_eq!(parse_browser(FIREFOX_LINUX), Some("Firefox"));
        assert_eq!(parse_browser(CHROME_ANDROID), Some("Chrome"));
    }

    #[test]
    fn os_detection_respects_ua_nesting() {
        // iPad 的 UA 含 "Mac OS X"；Android 与 ChromeOS 的含 "Linux"
        assert_eq!(parse_os(IPAD), Some("iPadOS"));
        assert_eq!(parse_os(SAFARI_IOS), Some("iOS"));
        assert_eq!(parse_os(CHROME_ANDROID), Some("Android"));
        assert_eq!(parse_os(CHROMEOS), Some("ChromeOS"));
        assert_eq!(parse_os(CHROME_WIN), Some("Windows"));
        assert_eq!(parse_os(SAFARI_MAC), Some("macOS"));
        assert_eq!(parse_os(FIREFOX_LINUX), Some("Linux"));
    }

    #[test]
    fn unknown_ua_returns_none_not_a_guess() {
        // 猜错一个冷门系统名，不如干脆不显示那一行
        for ua in ["", "curl/8.4.0", "Some-Bot/1.0", "garbage"] {
            assert_eq!(parse_os(ua), None, "不该猜 {ua:?} 的系统");
            assert_eq!(parse_browser(ua), None, "不该猜 {ua:?} 的浏览器");
        }
    }

    #[test]
    fn parsing_never_panics_on_hostile_input() {
        for ua in ["\0\0\0", &"A".repeat(10_000), "中文 UA 🎉", "Windows NT"] {
            let _ = parse_os(ua);
            let _ = parse_browser(ua);
        }
    }
}
