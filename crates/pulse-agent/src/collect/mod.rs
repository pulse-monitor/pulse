//! 指标采集。
//!
//! 一个 [`Collector`] 接口，三份平台实现。原则：
//!
//! **能拿到就拿，拿不到就报 `None` 并在 [`Capabilities`] 里如实声明。**
//! 绝不为了凑齐字段而提权、也绝不调外部命令 —— 拿不到就让前端隐藏这个字段，
//! 显示一个 0 是撒谎。

pub mod filter;
pub mod gpu;

// `linux` 模块**在任何平台都编译**：它的 `parse` 子模块是纯函数，
// 在 macOS 上也能跑完整的单元测试。解析格式出错的概率远高于文件读不到，
// 所以这部分必须在开发机上就验到底。采集实现本身才按平台门控。
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

use pulse_proto::{Capabilities, Metrics, RuntimeConfig};

#[cfg(target_os = "linux")]
pub use linux::LinuxCollector as Platform;
#[cfg(target_os = "macos")]
pub use macos::MacosCollector as Platform;
#[cfg(target_os = "windows")]
pub use windows::WindowsCollector as Platform;

/// 连接时上报一次的静态信息。
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub hostname: String,
    pub os: String,
    pub kernel: Option<String>,
    pub arch: String,
    pub cpu_model: Option<String>,
    pub cpu_cores: u32,
    pub virtualization: Option<String>,
    pub mem_total: u64,
    pub swap_total: u64,
    pub disk_total: u64,
    pub boot_at: i64,
    pub interfaces: Vec<String>,
    pub capabilities: Capabilities,
}

pub trait Collector: Send {
    /// 静态信息与能力声明。每次重连时重新探测 —— 机器可能加了显卡、
    /// 管理员可能改了 `ping_group_range`。
    fn facts(&mut self) -> Facts;

    /// 一次采样。**永不 panic**：单个指标取不到就留 `None`。
    fn sample(&mut self, cfg: &RuntimeConfig) -> Metrics;
}

/// 由两次累计计数与时间差算速率。
///
/// 计数器回绕（机器重启、网卡重置）时返回 0 而不是一个天文数字 ——
/// 前端上出现一条 8 EB/s 的尖峰比没有数据更糟。
pub fn rate(prev: u64, cur: u64, secs: f64) -> u64 {
    if secs <= 0.0 || cur < prev {
        return 0;
    }
    ((cur - prev) as f64 / secs) as u64
}

/// 负载均值的定点表示（×100）。
///
/// **必须 round 不能 truncate**：f32 里 `0.59 * 100.0` 是 58.99999…，
/// 直接 `as u16` 会把负载 0.59 报成 0.58。这类误差每个小数都会中招。
///
/// 同时挡住非有限值与负数（异常内核），并 clamp 到 u16 上界 ——
/// 机器抖死时负载能上千，655.36 就溢出了。
// Windows 没有 load average 概念，那里用不到这个函数
#[cfg_attr(target_os = "windows", allow(dead_code))]
pub fn load_fixed(v: f32) -> u16 {
    // NaN 不带信息 → 0；负数（含 -∞）→ 0；+∞ 交给 clamp 落到上界。
    // 与 pulse_proto::pct_to_basis_points 保持同一套语义 ——
    // 把 NaN 和 ±∞ 一视同仁地早退是这里踩过两次的坑。
    if v.is_nan() || v <= 0.0 {
        return 0;
    }
    (v * 100.0).round().min(f32::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_fixed_rounds_not_truncates() {
        // 回归测试：0.59 曾被报成 0.58
        assert_eq!(load_fixed(0.59), 59);
        assert_eq!(load_fixed(0.52), 52);
        assert_eq!(load_fixed(0.58), 58);
        assert_eq!(load_fixed(1.0), 100);
        assert_eq!(load_fixed(0.0), 0);
        // 逐个验证两位小数，任何一个截断都会被抓到
        for i in 0..=200u16 {
            assert_eq!(load_fixed(f32::from(i) / 100.0), i, "load {i}/100 转换错误");
        }
    }

    #[test]
    fn load_fixed_handles_hostile_values() {
        assert_eq!(load_fixed(-1.0), 0);
        assert_eq!(load_fixed(f32::NAN), 0);
        assert_eq!(load_fixed(f32::INFINITY), u16::MAX);
        // 机器抖死时负载能上千，655.36 就溢出了
        assert_eq!(load_fixed(9999.0), u16::MAX);
    }

    #[test]
    fn rate_basic() {
        assert_eq!(rate(0, 1000, 1.0), 1000);
        assert_eq!(rate(1000, 3000, 2.0), 1000);
    }

    #[test]
    fn rate_handles_counter_reset() {
        // 机器重启后计数器归零：不能算出一个天文数字的负速率
        assert_eq!(rate(1_000_000, 5, 1.0), 0);
    }

    #[test]
    fn rate_handles_zero_or_negative_interval() {
        assert_eq!(rate(0, 1000, 0.0), 0);
        assert_eq!(rate(0, 1000, -1.0), 0);
    }
}
