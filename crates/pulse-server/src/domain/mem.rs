//! 内存口径（R3 的「包含缓冲区内存」开关）。
//!
//! agent 上报**原始三元组**，口径在这里算 —— 所以后台改开关时
//! 历史数据也一并按新口径重新呈现，不需要重装 agent。

/// 「已用内存」的两种口径。
///
/// - `false`（默认）：`total - available`，即「应用还能拿到多少」，
///   对应 Linux 内核的 `MemAvailable`
/// - `true`：`total - free`，与 `free` 命令的 `used + buff/cache` 一致
///
/// 全部用 `saturating_sub`：异常内核可能给出 `available > total`，
/// 那时应当得到 0 而不是一个环绕过的天文数字。
pub fn used(total: u64, free: u64, available: u64, include_buffcache: bool) -> u64 {
    if include_buffcache {
        total.saturating_sub(free)
    } else {
        total.saturating_sub(available)
    }
}

/// 使用率百分比。`total` 为 0（采集失败）时返回 `None` ——
/// 返回 0% 会让前端画出一条假的贴地线，而正确的表现是显示 `—`。
pub fn pct(used: u64, total: u64) -> Option<f32> {
    (total > 0).then(|| (used as f64 * 100.0 / total as f64) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: u64 = 1 << 30;

    #[test]
    fn two_modes_give_different_numbers_on_linux() {
        // 典型 Linux：4 GiB 总量，178 MiB 空闲，2.8 GiB 可用
        let (total, free, avail) = (4 * G, 182_364 * 1024, 2_938_104 * 1024);
        let strict = used(total, free, avail, false);
        let with_cache = used(total, free, avail, true);

        assert!(with_cache > strict, "含缓冲的口径必然更大");
        assert_eq!(strict, total - avail);
        assert_eq!(with_cache, total - free);
    }

    #[test]
    fn windows_where_available_equals_free_gives_identical_results() {
        // Windows 没有 buff/cache 概念，两种口径结果相同 ——
        // 前端应当据 capabilities 把这个开关灰掉
        let (total, free) = (8 * G, 3 * G);
        assert_eq!(
            used(total, free, free, false),
            used(total, free, free, true)
        );
    }

    #[test]
    fn abnormal_kernel_values_clamp_to_zero_not_wrap() {
        // available > total 时不能环绕成天文数字
        assert_eq!(used(G, 0, 2 * G, false), 0);
        assert_eq!(used(0, 0, 0, false), 0);
        assert_eq!(used(G, 2 * G, 0, true), 0);
    }

    #[test]
    fn pct_of_unknown_total_is_none_not_zero() {
        // 采集失败时前端要显示「—」，不是「0%」
        assert_eq!(pct(0, 0), None);
        assert_eq!(pct(100, 0), None);
        assert_eq!(pct(G, 4 * G), Some(25.0));
    }
}
