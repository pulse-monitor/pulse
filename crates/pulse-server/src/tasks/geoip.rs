//! GeoIP 表的拉取与缓存。
//!
//! 数据源是 **DB-IP Lite**（经 ip-location-db 分发，CC-BY-4.0）。
//! **IP 查询全在本地做** —— 用第三方 API 等于把用户所有 VPS 的 IP
//! 挨个报给别人，一个监控面板不该这么干。
//!
//! 缓存策略：CSV 落到 `<数据目录>/geoip/`，启动时先用缓存把表建起来
//! （**离线也能用**），再决定要不要刷新。数据库每月更新，一周查一次足够。
//!
//! 归属：本产品包含 DB-IP.com 创建的 IP 地理定位数据，依 CC-BY-4.0 授权使用。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use tracing::{info, warn};

use crate::domain::geoip::GeoTable;

/// DB-IP Lite 的国家级 CSV，v4 与 v6 各一份。
const SOURCES: [(&str, &str); 2] = [
    (
        "v4",
        "https://raw.githubusercontent.com/sapics/ip-location-db/main/dbip-country/dbip-country-ipv4.csv",
    ),
    (
        "v6",
        "https://raw.githubusercontent.com/sapics/ip-location-db/main/dbip-country/dbip-country-ipv6.csv",
    ),
];

/// 单个文件的拉取超时。这些文件几 MB，慢一点正常，但不能无限等。
const FETCH_TIMEOUT: Duration = Duration::from_secs(180);
/// 缓存多久算过期。
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

/// 全局可读的表。用 `ArcSwap` 换表 —— 刷新时不阻塞查询。
pub type SharedTable = Arc<ArcSwap<GeoTable>>;

fn cache_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("geoip")
}

/// 从缓存目录把表建起来。缓存不存在时返回空表（查询一律返回 None）。
pub fn load_cached(data_dir: &Path) -> GeoTable {
    let dir = cache_dir(data_dir);
    let mut files = Vec::new();
    for (name, _) in SOURCES {
        match std::fs::read_to_string(dir.join(format!("{name}.csv"))) {
            Ok(s) => files.push(s),
            // 缺一份不影响另一份 —— 只有 v4 也好过完全没有
            Err(_) => continue,
        }
    }
    if files.is_empty() {
        return GeoTable::default();
    }
    let t = GeoTable::parse(files);
    let (v4, v6) = t.len();
    info!(v4, v6, "已从缓存载入 GeoIP 表");
    t
}

/// 缓存是不是该刷新了。
fn stale(data_dir: &Path, now: std::time::SystemTime) -> bool {
    let dir = cache_dir(data_dir);
    for (name, _) in SOURCES {
        let Ok(m) = std::fs::metadata(dir.join(format!("{name}.csv"))) else {
            return true; // 缺文件 → 要拉
        };
        let Ok(t) = m.modified() else { return true };
        match now.duration_since(t) {
            Ok(age) if age > MAX_AGE => return true,
            // 时钟回拨导致的「未来时间」不当成过期，否则会每次启动都重拉
            Err(_) => continue,
            _ => {}
        }
    }
    false
}

/// 拉一轮。任何一家失败都只记日志，不影响其余四家，也不清空已有缓存。
pub async fn fetch_once(data_dir: &Path) -> anyhow::Result<()> {
    let dir = cache_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    let client = reqwest::Client::builder().timeout(FETCH_TIMEOUT).build()?;

    let mut ok = 0;
    for (name, url) in SOURCES {
        match client.get(url).send().await {
            Ok(r) if r.status().is_success() => match r.text().await {
                Ok(body) if body.len() > 1024 => {
                    // 先写临时文件再改名：中断时不会留下半个文件被当成有效缓存
                    let tmp = dir.join(format!("{name}.tmp"));
                    if let Err(e) = std::fs::write(&tmp, &body) {
                        warn!(%name, "写 GeoIP 缓存失败: {e}");
                        continue;
                    }
                    if let Err(e) = std::fs::rename(&tmp, dir.join(format!("{name}.csv"))) {
                        warn!(%name, "替换 GeoIP 缓存失败: {e}");
                        continue;
                    }
                    ok += 1;
                }
                Ok(_) => warn!(%name, "GeoIP 响应太短，疑似错误页，保留旧缓存"),
                Err(e) => warn!(%name, "读取 GeoIP 响应失败: {e}"),
            },
            Ok(r) => warn!(%name, status = %r.status(), "GeoIP 源返回非 2xx，保留旧缓存"),
            Err(e) => warn!(%name, "GeoIP 源拉取失败: {e}"),
        }
    }
    info!(ok, total = SOURCES.len(), "GeoIP 源拉取完成");
    Ok(())
}

/// 后台任务：启动时先用缓存，必要时刷新，之后每天检查一次。
pub fn spawn(data_dir: PathBuf, table: SharedTable) {
    tokio::spawn(async move {
        loop {
            if stale(&data_dir, std::time::SystemTime::now()) {
                if let Err(e) = fetch_once(&data_dir).await {
                    warn!("GeoIP 拉取出错: {e}");
                }
                table.store(Arc::new(load_cached(&data_dir)));
            }
            // 一天检查一次。真正的下载只在缓存超过一周时才发生
            tokio::time::sleep(Duration::from_secs(24 * 3600)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_cache_is_stale_and_loads_as_empty() {
        let d = tempfile::tempdir().unwrap();
        assert!(stale(d.path(), std::time::SystemTime::now()));
        assert!(load_cached(d.path()).is_empty());
    }

    #[test]
    fn fresh_cache_is_not_stale() {
        let d = tempfile::tempdir().unwrap();
        let dir = cache_dir(d.path());
        std::fs::create_dir_all(&dir).unwrap();
        for (name, _) in SOURCES {
            std::fs::write(dir.join(format!("{name}.csv")), "x").unwrap();
        }
        assert!(!stale(d.path(), std::time::SystemTime::now()));
    }

    #[test]
    fn a_missing_source_does_not_lose_the_others() {
        // 只有 v4 缓存时，v4 的查询仍然要能用
        let d = tempfile::tempdir().unwrap();
        let dir = cache_dir(d.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("v4.csv"), "1.0.16.0,1.0.31.255,JP\n").unwrap();
        let t = load_cached(d.path());
        assert_eq!(t.lookup("1.0.16.1".parse().unwrap()), Some("JP"));
    }

    #[test]
    fn clock_skew_does_not_force_a_refetch_every_boot() {
        // 缓存文件的 mtime 在未来（时钟回拨过）时不该被当成过期
        let d = tempfile::tempdir().unwrap();
        let dir = cache_dir(d.path());
        std::fs::create_dir_all(&dir).unwrap();
        for (name, _) in SOURCES {
            std::fs::write(dir.join(format!("{name}.csv")), "x").unwrap();
        }
        let past = std::time::SystemTime::now() - Duration::from_secs(3600);
        assert!(!stale(d.path(), past));
    }
}
