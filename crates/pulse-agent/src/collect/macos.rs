//! macOS 采集。
//!
//! 走 `sysinfo`（内部用 mach / sysctl，无外部命令）。
//! 拿不到的两项如实声明为不可用，的三平台分界表：
//! - **温度**：需要 SMC 特权访问
//! - **TCP 连接数**：需要特权

use std::collections::HashSet;
use std::time::{Duration, Instant};

use pulse_proto::{Capabilities, DiskUsage, Mem, Metrics, NetStat, RuntimeConfig};
use sysinfo::{
    CpuRefreshKind, Disks, MemoryRefreshKind, Networks, ProcessRefreshKind, ProcessesToUpdate,
    RefreshKind, System,
};

use super::{filter, gpu::Gpu, load_fixed, rate, Collector, Facts};

pub struct MacosCollector {
    sys: System,
    disks: Disks,
    nets: Networks,
    gpu: Gpu,
    prev_net: Option<(u64, u64, Instant)>,
    /// 进程枚举比其他指标贵得多，按更低的频率刷新并缓存。
    procs: Option<(u32, Instant)>,
}

impl MacosCollector {
    pub fn new() -> Self {
        let mut sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
                .with_memory(MemoryRefreshKind::everything()),
        );
        sys.refresh_cpu_usage();
        Self {
            sys,
            disks: Disks::new_with_refreshed_list(),
            nets: Networks::new_with_refreshed_list(),
            gpu: Gpu::probe(),
            prev_net: None,
            procs: None,
        }
    }

    /// 磁盘合计。**facts 与 sample 必须共用这一个实现** ——
    /// 之前 facts 里是一句没去重的 `.map(total_space).sum`，
    /// 于是「静态信息里的磁盘总量」和「实时指标里的磁盘总量」对不上。
    fn disk_usage(&self, cfg: &RuntimeConfig) -> DiskUsage {
        let mounts: Vec<String> = self
            .disks
            .iter()
            .map(|d| d.mount_point().display().to_string())
            .collect();
        let picked = filter::select(&mounts, &cfg.disk_include, &cfg.disk_exclude);

        let mut seen = HashSet::new();
        let (mut total, mut used) = (0u64, 0u64);
        for d in self.disks.iter() {
            let mp = d.mount_point().display().to_string();
            if !picked.contains(&&mp) {
                continue;
            }
            if !counts_toward_disk(&d.file_system().to_string_lossy(), d.is_read_only()) {
                continue;
            }
            // 二道保险：万一同容器里出现两个可写卷，按容量数字再去重一次
            if !seen.insert((d.total_space(), d.available_space())) {
                continue;
            }
            total += d.total_space();
            used += d.total_space().saturating_sub(d.available_space());
        }
        DiskUsage { total, used }
    }

    /// 进程数。枚举全部进程比其他指标贵一个数量级，所以按 [`PROC_REFRESH`]
    /// 的间隔刷新并缓存 —— 2 秒一次会让 agent 的 CPU 占用明显上去。
    fn process_count(&mut self) -> u32 {
        const PROC_REFRESH: Duration = Duration::from_secs(15);
        let stale = self
            .procs
            .is_none_or(|(_, at)| at.elapsed() >= PROC_REFRESH);
        if stale {
            self.sys.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing(),
            );
            self.procs = Some((self.sys.processes().len() as u32, Instant::now()));
        }
        self.procs.map(|(n, _)| n).unwrap_or(0)
    }
}

/// 判断一个卷是否应当计入磁盘统计。
///
/// APFS 的多个卷共享同一个容器，**每个卷都汇报整个容器的总量**。
/// 不处理的话一块 228 GiB 的盘会被 `/` 和 `/System/Volumes/Data` 各算一遍。
///
/// 判据是**跳过只读卷**，而不是比较容量数字：
/// - macOS 的 `/` 是只读系统卷、`/System/Volumes/Data` 是可写数据卷，
///   同属一个容器；只读卷占的空间本来就已经算在数据卷里了
/// - 试过用 `(总量, 剩余量)` 作为去重键，但**剩余量会在两次 refresh 之间波动**，
///   一旦不再完全相等去重就失效 —— 实测出现过 5114 GiB 与 4886 GiB 交替出现
/// - `is_read_only` 是稳定属性，不会抖
///
/// 代价：用户手动挂载的只读数据卷（如 ISO、只读 NFS）不计入。
/// 对探针来说可以接受 —— 它们的占用本来就是静态的。
fn counts_toward_disk(fs: &str, read_only: bool) -> bool {
    !read_only && filter::is_real_filesystem(fs)
}

impl Default for MacosCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for MacosCollector {
    fn facts(&mut self) -> Facts {
        self.sys.refresh_memory();
        self.disks.refresh(true);
        self.nets.refresh(true);

        Facts {
            hostname: System::host_name().unwrap_or_else(|| "unknown".into()),
            os: System::long_os_version().unwrap_or_else(|| "macOS".into()),
            kernel: System::kernel_version(),
            arch: System::cpu_arch(),
            cpu_model: self
                .sys
                .cpus()
                .first()
                .map(|c| c.brand().trim().to_string()),
            cpu_cores: System::physical_core_count().unwrap_or(0) as u32,
            virtualization: None, // macOS 上恒为物理机语义
            mem_total: self.sys.total_memory(),
            swap_total: self.sys.total_swap(),
            disk_total: self.disk_usage(&RuntimeConfig::default()).total,
            boot_at: System::boot_time() as i64,
            interfaces: self.nets.keys().cloned().collect(),
            capabilities: Capabilities {
                gpu_nvml: self.gpu.available(),
                temperature: false,    // 需要 SMC 特权
                tcp_conn_count: false, // 需要特权
                proc_count: true,
                load_average: true,
                icmp_unprivileged: false, // macOS 无 ping_group_range；延迟监控走 TCP
                self_update: crate::update::available(),
                cgroup_limited: false,
            },
        }
    }

    fn sample(&mut self, cfg: &RuntimeConfig) -> Metrics {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.disks.refresh(false);
        self.nets.refresh(false);

        let load = System::load_average();
        let names: Vec<String> = self.nets.keys().cloned().collect();
        let picked = filter::select(&names, &cfg.net_include, &cfg.net_exclude);
        let (mut rx, mut tx) = (0u64, 0u64);
        for (name, data) in self.nets.iter() {
            if picked.contains(&name) {
                rx += data.total_received();
                tx += data.total_transmitted();
            }
        }
        let now = Instant::now();
        let (rx_speed, tx_speed) = match self.prev_net.replace((rx, tx, now)) {
            Some((prx, ptx, pt)) => {
                let s = now.duration_since(pt).as_secs_f64();
                (rate(prx, rx, s), rate(ptx, tx, s))
            }
            None => (0, 0),
        };

        let disk = self.disk_usage(cfg);

        let total = self.sys.total_memory();
        let free = self.sys.free_memory();
        // sysinfo 的 available = free + inactive + purgeable − compressor，用的是
        // saturating_sub —— 内存压力大时压缩页超过前三项就直接饱和成 0，
        // 于是「已用」变成 100%。回落到 total − used（即 Activity Monitor 的
        // 「已用内存」= active + wire + compressor + speculative），这才是
        // 「应用还能再拿到多少」的正确口径。
        let available = self
            .sys
            .available_memory()
            .max(total.saturating_sub(self.sys.used_memory()));
        Metrics {
            ts: super::super::now_unix(),
            cpu_pct: pulse_proto::pct_to_basis_points(self.sys.global_cpu_usage()),
            load: [
                load_fixed(load.one as f32),
                load_fixed(load.five as f32),
                load_fixed(load.fifteen as f32),
            ],
            mem: Mem {
                total,
                free,
                available,
                buffers: 0, // macOS 无对应概念
                cached: 0,
                swap_total: self.sys.total_swap(),
                swap_free: self.sys.free_swap(),
            },
            disk,
            net: NetStat {
                rx_bytes: rx,
                tx_bytes: tx,
                rx_speed,
                tx_speed,
                ifaces: picked.into_iter().cloned().collect(),
            },
            tcp_conn: None, // 需要特权，如实报 None 而不是 0
            udp_conn: None,
            proc_count: Some(self.process_count()),
            gpu: cfg.gpu_enabled.then(|| self.gpu.sample()).flatten(),
            cpu_temp: None, // 需要 SMC 特权
            uptime_s: System::uptime(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_system_volume_is_not_counted_twice() {
        // 本机实测（sysinfo 0.36，macOS 26.5）：
        //   name="Macintosh HD" fs=apfs  mount="/"                    total=245107195904 ro=true
        //   name="Macintosh HD" fs=apfs  mount="/System/Volumes/Data" total=245107195904 ro=false
        //   name="My Passport"  fs=exfat mount="/Volumes/My Passport" total=5000844804096 ro=false
        // 只有后两个应当计入，合计 4886 GiB —— 与 df 一致。
        assert!(!counts_toward_disk("apfs", true), "只读系统卷不计入");
        assert!(counts_toward_disk("apfs", false), "可写数据卷计入");
        assert!(counts_toward_disk("exfat", false), "外接盘计入");
    }

    #[test]
    fn virtual_filesystems_are_never_counted() {
        // devfs / autofs 之类算进去会让磁盘总量凭空变大
        assert!(!counts_toward_disk("devfs", false));
        assert!(!counts_toward_disk("autofs", false));
        assert!(!counts_toward_disk("tmpfs", false));
    }
}
