//! Windows 采集。
//!
//! 走 `sysinfo`（内部用 Win32 API，**不调 WMI、不调 PowerShell** —— R18）。
//! 拿不到的两项如实声明，的三平台分界表：
//! - **负载**：Windows 没有 load average 这个概念
//! - **温度**：需要 WMI + 管理员权限

use std::time::Instant;

use pulse_proto::{Capabilities, DiskUsage, Mem, Metrics, NetStat, RuntimeConfig};
use sysinfo::{CpuRefreshKind, Disks, MemoryRefreshKind, Networks, RefreshKind, System};

use super::{filter, gpu::Gpu, rate, Collector, Facts};

pub struct WindowsCollector {
    sys: System,
    disks: Disks,
    nets: Networks,
    gpu: Gpu,
    prev_net: Option<(u64, u64, Instant)>,
}

impl WindowsCollector {
    pub fn new() -> Self {
        let mut sys = System::new_with_specifics(
            RefreshKind::nothing
                .with_cpu(CpuRefreshKind::nothing.with_cpu_usage)
                .with_memory(MemoryRefreshKind::everything),
        );
        sys.refresh_cpu_usage;
        Self {
            sys,
            disks: Disks::new_with_refreshed_list,
            nets: Networks::new_with_refreshed_list,
            gpu: Gpu::probe,
            prev_net: None,
        }
    }
}

impl Default for WindowsCollector {
    fn default() -> Self {
        Self::new
    }
}

impl Collector for WindowsCollector {
    fn facts(&mut self) -> Facts {
        self.sys.refresh_memory;
        self.disks.refresh(true);
        self.nets.refresh(true);

        Facts {
            hostname: System::host_name.unwrap_or_else(|| "unknown".into),
            os: System::long_os_version.unwrap_or_else(|| "Windows".into),
            kernel: System::kernel_version,
            arch: System::cpu_arch,
            cpu_model: self.sys.cpus.first.map(|c| c.brand.trim().to_string()),
            cpu_cores: System::physical_core_count.unwrap_or(0) as u32,
            virtualization: None,
            mem_total: self.sys.total_memory,
            swap_total: self.sys.total_swap,
            disk_total: self.disks.iter().map(|d| d.total_space).sum,
            boot_at: System::boot_time as i64,
            interfaces: self.nets.keys.cloned.collect,
            capabilities: Capabilities {
                gpu_nvml: self.gpu.available,
                temperature: false,  // 需要 WMI + 管理员
                load_average: false, // Windows 没有这个概念
                tcp_conn_count: false,
                proc_count: true,
                icmp_unprivileged: false, // 延迟监控走 TCP
                self_update: crate::update::available,
                cgroup_limited: false,
            },
        }
    }

    fn sample(&mut self, cfg: &RuntimeConfig) -> Metrics {
        self.sys.refresh_cpu_usage;
        self.sys.refresh_memory;
        self.disks.refresh(false);
        self.nets.refresh(false);

        let names: Vec<String> = self.nets.keys.cloned.collect;
        let picked = filter::select(&names, &cfg.net_include, &cfg.net_exclude);
        let (mut rx, mut tx) = (0u64, 0u64);
        for (name, data) in self.nets.iter() {
            if picked.contains(&name) {
                rx += data.total_received;
                tx += data.total_transmitted;
            }
        }
        let now = Instant::now();
        let (rx_speed, tx_speed) = match self.prev_net.replace((rx, tx, now)) {
            Some((prx, ptx, pt)) => {
                let s = now.duration_since(pt).as_secs_f64;
                (rate(prx, rx, s), rate(ptx, tx, s))
            }
            None => (0, 0),
        };

        let mount_names: Vec<String> = self
            .disks
            .iter()
            .map(|d| d.mount_point.display.to_string())
            .collect;
        let picked_mounts = filter::select(&mount_names, &cfg.disk_include, &cfg.disk_exclude);
        let (mut dt, mut du) = (0u64, 0u64);
        for d in self.disks.iter() {
            let mp = d.mount_point.display.to_string();
            if picked_mounts.contains(&&mp) {
                dt += d.total_space;
                du += d.total_space.saturating_sub(d.available_space);
            }
        }

        let total = self.sys.total_memory;
        Metrics {
            ts: crate::now_unix(),
            cpu_pct: pulse_proto::pct_to_basis_points(self.sys.global_cpu_usage),
            load: [0; 3], // capabilities.load_average = false，前端会隐藏
            mem: Mem {
                total,
                free: self.sys.free_memory,
                // Windows 的 ullAvailPhys 就是 available 语义；无 buff/cache 概念，
                // 所以「含缓冲」开关在这台机器上两种口径结果相同
                available: self.sys.available_memory,
                buffers: 0,
                cached: 0,
                swap_total: self.sys.total_swap,
                swap_free: self.sys.free_swap,
            },
            disk: DiskUsage {
                total: dt,
                used: du,
            },
            net: NetStat {
                rx_bytes: rx,
                tx_bytes: tx,
                rx_speed,
                tx_speed,
                ifaces: picked.into_iter.cloned.collect,
            },
            tcp_conn: None,
            udp_conn: None,
            proc_count: Some(self.sys.processes.len as u32),
            gpu: cfg.gpu_enabled.then(|| self.gpu.sample).flatten,
            cpu_temp: None,
            uptime_s: System::uptime,
        }
    }
}
