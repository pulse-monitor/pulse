//! 汇率拉取任务。
//!
//! 源是自建的 Frankfurter（`rate.jinqians.com`）。**已实测的限制**：
//! 只覆盖 30 种货币、每工作日更新一次。VPS 常见但不覆盖的
//! （RUB / TWD / VND / UAH / ARS / AED）走人工汇率。

use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use crate::domain::rate::FrankfurterLatest;
use crate::store::Storage;

/// 拉取超时。源挂掉时不能让任务卡住整个周期。
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

pub async fn fetch_once(store: &Arc<dyn Storage>, base_url: &str, now: i64) -> anyhow::Result<()> {
    let url = format!("{}/v1/latest?base=USD", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder().timeout(FETCH_TIMEOUT).build()?;

    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            // L1 降级：拉取失败就继续用库里的旧值。
            // **不清空、不报错退出** —— 有旧汇率远好过没有汇率
            warn!("汇率拉取失败，继续使用已缓存的值: {e}");
            return Ok(());
        }
    };
    if !resp.status().is_success() {
        warn!(status = %resp.status(), "汇率接口返回非 2xx，继续使用已缓存的值");
        return Ok(());
    }

    let parsed: FrankfurterLatest = match resp.json().await {
        Ok(p) => p,
        Err(e) => {
            warn!("汇率响应解析失败，继续使用已缓存的值: {e}");
            return Ok(());
        }
    };
    let Some(rates) = parsed.into_rates(now) else {
        // base 不是 USD 时整张表的语义就变了，与其算错不如不更新
        warn!("汇率响应的 base 不是 USD，已忽略本次更新");
        return Ok(());
    };

    let n = rates.len();
    store.save_rates(&rates, now).await?;
    info!(currencies = n, as_of = %rates.as_of, "汇率已更新");
    Ok(())
}
