//! GPU 采集。
//!
//! 安全约束（R18）：**动态加载 NVML 库，绝不调用 `nvidia-smi` 子进程**。
//! agent 里不存在任何执行外部命令的路径 —— CI 有断言守着。
//!
//! M2 先把接口与降级路径定下来，NVML 绑定在 `gpu` feature 里补齐；
//! 未编译该 feature 或运行时加载失败时静默返回 `None`，
//! 并在 `capabilities.gpu_nvml` 里如实报 false。

use pulse_proto::GpuStat;

#[derive(Default)]
pub struct Gpu {
    available: bool,
}

impl Gpu {
    /// 运行时探测，不是按平台猜。
    pub fn probe() -> Self {
        Self {
            available: probe_nvml(),
        }
    }

    pub fn available(&self) -> bool {
        self.available
    }

    pub fn sample(&mut self) -> Option<GpuStat> {
        if !self.available {
            return None;
        }
        sample_nvml()
    }
}

#[cfg(not(feature = "gpu"))]
fn probe_nvml() -> bool {
    false
}

#[cfg(not(feature = "gpu"))]
fn sample_nvml() -> Option<GpuStat> {
    None
}

#[cfg(feature = "gpu")]
fn probe_nvml() -> bool {
    // nvml-wrapper 会 dlopen libnvidia-ml.so.1；失败即视为无 GPU。
    // 这里刻意不打 error 日志 —— 绝大多数机器本来就没有 N 卡。
    nvml_wrapper::Nvml::init().is_ok()
}

#[cfg(feature = "gpu")]
fn sample_nvml() -> Option<GpuStat> {
    let nvml = nvml_wrapper::Nvml::init().ok()?;
    let dev = nvml.device_by_index(0).ok()?;
    let util = dev.utilization_rates().ok()?;
    let mem = dev.memory_info().ok()?;
    Some(GpuStat {
        util: pulse_proto::pct_to_basis_points(util.gpu as f32),
        mem_used: mem.used,
        mem_total: mem.total,
        temp: dev
            .temperature(nvml_wrapper::enum_wrappers::device::TemperatureSensor::Gpu)
            .map(|t| (t as i32) * 10)
            .unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_degrades_silently_without_gpu() {
        // 开发机上没有 N 卡：探测必须安静地返回不可用，而不是报错或 panic
        let mut g = Gpu::probe();
        if !g.available() {
            assert!(g.sample().is_none());
        }
    }
}
