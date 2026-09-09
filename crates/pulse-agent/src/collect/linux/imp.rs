//! Linux 采集：读文件 + 调 [`super::parse`] 的纯函数。
//!
//! **文件根路径可注入**（`PULSE_PROC` / `PULSE_SYS` / `PULSE_ROOTFS`），
//! 这既支持 Docker 挂载宿主机文件系统，也让组装逻辑（哪个文件填哪个字段）
//! 能用 fixture 目录在任何平台上测 —— 真机只需回答「这些文件读不读得到」。
//!
//! 权限逐项审计全部路径在非 root 下均可读。

use std::path::{Path, PathBuf};
use std::time::Instant;

use pulse_proto::{Capabilities, DiskUsage, Mem, Metrics, NetStat, RuntimeConfig};

use super::parse::{self, CpuTimes};
use crate::collect::{filter, gpu::Gpu, load_fixed, rate, Collector, Facts};

/// 文件系统根路径。默认指向真实的 `/proc` `/sys`。
#[derive(Debug, Clone)]
pub struct Roots {
    pub proc: PathBuf,
    pub sys: PathBuf,
    /// 磁盘挂载点的前缀。Docker 里挂了宿主机根目录时设为 `/rootfs`。
    pub rootfs: PathBuf,
}

impl Default for Roots {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Roots {
    pub fn from_env() -> Self {
        let get = |k: &str, d: &str| PathBuf::from(std::env::var(k).unwrap_or_else(|_| d.into()));
        Self {
            proc: get("PULSE_PROC", "/proc"),
            sys: get("PULSE_SYS", "/sys"),
            rootfs: get("PULSE_ROOTFS", ""),
        }
    }

    fn read(&self, base: &Path, rel: &str) -> Option<String> {
        std::fs::read_to_string(base.join(rel)).ok()
    }
    fn proc(&self, rel: &str) -> Option<String> {
        self.read(&self.proc, rel)
    }
    fn sys(&self, rel: &str) -> Option<String> {
        self.read(&self.sys, rel)
    }
}

pub struct LinuxCollector {
    roots: Roots,
    gpu: Gpu,
    prev_cpu: Option<CpuTimes>,
    prev_net: Option<(u64, u64, Instant)>,
    caps: Capabilities,
    /// 容器里 cgroup 给出的内存上限。存在时优先于 `/proc/meminfo` 的宿主机数值。
    cgroup_mem: Option<u64>,
}

impl LinuxCollector {
    pub fn new() -> Self {
        Self::with_roots(Roots::from_env())
    }

    pub fn with_roots(roots: Roots) -> Self {
        let gpu = Gpu::probe();
        let mut c = Self {
            roots,
            gpu,
            prev_cpu: None,
            prev_net: None,
            caps: Capabilities::default(),
            cgroup_mem: None,
        };
        c.probe_capabilities();
        // 预热一次 CPU 采样，否则第一个值没有意义
        c.prev_cpu = c.roots.proc("stat").as_deref().and_then(parse::cpu_times);
        c
    }

    /// 运行时逐项探测，不是按平台猜 —— 同样是 Linux，KVM 上有温度传感器
    /// 而容器里没有，`/proc` 挂了 hidepid 的机器上进程数只能看到自己的。
    fn probe_capabilities(&mut self) {
        let hidepid = self
            .roots
            .proc("mounts")
            .as_deref()
            .map(parse::proc_has_hidepid)
            .unwrap_or(false);
        // hidepid 之外还有一种情况：ProtectProc=invisible 下看不到 PID 1
        let sees_init = self.roots.proc.join("1").exists();

        let icmp = self
            .roots
            .proc("sys/net/ipv4/ping_group_range")
            .map(|r| parse::ping_group_allows(&r, &current_gids()))
            .unwrap_or(false);

        self.cgroup_mem = self.read_cgroup_mem_limit();
        // 有 cgroup 限额且没挂 lxcfs → /proc 里是宿主机数据，必须用 cgroup 值
        let lxcfs = self
            .roots
            .proc("mounts")
            .map(|m| m.contains("lxcfs"))
            .unwrap_or(false);

        self.caps = Capabilities {
            icmp_unprivileged: icmp,
            gpu_nvml: self.gpu.available(),
            temperature: self.read_temp().is_some(),
            tcp_conn_count: self.roots.proc("net/sockstat").is_some(),
            proc_count: !hidepid && sees_init,
            load_average: self.roots.proc("loadavg").is_some(),
            self_update: crate::update::available(),
            cgroup_limited: self.cgroup_mem.is_some() && !lxcfs,
        };
    }

    /// 容器里的 CPU 配额（可用核数）。
    ///
    /// 与内存限额同一类坑：未挂 lxcfs 时 `/proc/cpuinfo` 数出来的是**宿主机**
    /// 的核数，一个限 0.5 核的容器会显示宿主机的 64 核。
    fn read_cgroup_cpu_quota(&self) -> Option<f64> {
        // cgroup v2
        if let Some(s) = self.read_cgroup("cpu.max") {
            if let Some(n) = parse::cgroup_cpu_max(&s) {
                return Some(n);
            }
        }
        // cgroup v1：quota 与 period 分在两个文件里
        let quota: f64 = self
            .read_cgroup("cpu/cpu.cfs_quota_us")?
            .trim()
            .parse()
            .ok()?;
        let period: f64 = self
            .read_cgroup("cpu/cpu.cfs_period_us")?
            .trim()
            .parse()
            .ok()?;
        (quota > 0.0 && period > 0.0).then(|| quota / period)
    }

    fn read_cgroup_mem_limit(&self) -> Option<u64> {
        // cgroup v2 优先，回落 v1
        for p in ["memory.max", "memory/memory.limit_in_bytes"] {
            if let Some(s) = self.read_cgroup(p) {
                if let Some(n) = parse::cgroup_limit(&s) {
                    return Some(n);
                }
            }
        }
        None
    }

    fn read_cgroup(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.roots.sys.join("fs/cgroup").join(rel)).ok()
    }

    /// 最高的那个传感器读数。先试 hwmon，再试 thermal_zone。
    ///
    /// 多数 KVM / 容器**根本没有传感器** —— 这与权限无关，
    /// 返回 `None` 后前端会隐藏温度字段。
    fn read_temp(&self) -> Option<i32> {
        let mut best: Option<i32> = None;
        for pattern in ["class/hwmon", "class/thermal"] {
            let Ok(dir) = std::fs::read_dir(self.roots.sys.join(pattern)) else {
                continue;
            };
            for e in dir.flatten() {
                let p = e.path();
                // hwmon*/temp*_input 与 thermal_zone*/temp
                let candidates: Vec<PathBuf> = match std::fs::read_dir(&p) {
                    Ok(inner) => inner
                        .flatten()
                        .map(|x| x.path())
                        .filter(|x| {
                            let n = x.file_name().and_then(|s| s.to_str()).unwrap_or("");
                            n == "temp" || (n.starts_with("temp") && n.ends_with("_input"))
                        })
                        .collect(),
                    Err(_) => continue,
                };
                for c in candidates {
                    if let Some(t) = std::fs::read_to_string(&c)
                        .ok()
                        .as_deref()
                        .and_then(parse::temp_millidegrees)
                    {
                        best = Some(best.map_or(t, |b: i32| b.max(t)));
                    }
                }
            }
        }
        best
    }

    fn read_mem(&self) -> Mem {
        let info = self
            .roots
            .proc("meminfo")
            .as_deref()
            .map(parse::meminfo)
            .unwrap_or_default();

        // 容器里未挂 lxcfs 时 /proc/meminfo 显示的是**宿主机**内存 ——
        // 一台 1 GB 的小鸡会显示宿主机的 128 GB。取 cgroup 限额与之较小者。
        let total = match self.cgroup_mem {
            Some(limit) if info.total == 0 => limit,
            Some(limit) => limit.min(info.total),
            None => info.total,
        };
        Mem {
            total,
            free: info.free.min(total),
            available: info.available_or_estimate().min(total),
            buffers: info.buffers,
            cached: info.cached + info.sreclaimable,
            swap_total: info.swap_total,
            swap_free: info.swap_free,
        }
    }

    fn read_disk(&self, cfg: &RuntimeConfig) -> DiskUsage {
        let Some(raw) = self.roots.proc("mounts") else {
            return DiskUsage::default();
        };
        let mounts = parse::mounts(&raw);

        let real: Vec<String> = mounts
            .iter()
            .filter(|m| filter::is_real_filesystem(&m.fstype))
            .map(|m| m.mount_point.clone())
            .collect();
        let picked = filter::select(&real, &cfg.disk_include, &cfg.disk_exclude);

        // 同一设备可能挂载多次（bind mount），按挂载点去重后仍可能重复计算，
        // 所以按设备去重：只统计每个块设备的第一个挂载点
        let mut seen_dev = std::collections::HashSet::new();
        let (mut total, mut used) = (0u64, 0u64);
        for m in &mounts {
            if !picked.iter().any(|p| **p == m.mount_point) {
                continue;
            }
            if !seen_dev.insert(m.device.clone()) {
                continue;
            }
            let path = if self.roots.rootfs.as_os_str().is_empty() {
                PathBuf::from(&m.mount_point)
            } else {
                self.roots
                    .rootfs
                    .join(m.mount_point.trim_start_matches('/'))
            };
            if let Some((t, u)) = statvfs(&path) {
                total += t;
                used += u;
            }
        }
        DiskUsage { total, used }
    }
}

impl Default for LinuxCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for LinuxCollector {
    fn facts(&mut self) -> Facts {
        self.probe_capabilities();
        let stat = self.roots.proc("stat");
        let cpuinfo = self.roots.proc("cpuinfo");
        let mem = self.read_mem();
        let ifaces = self
            .roots
            .proc("net/dev")
            .as_deref()
            .map(|s| parse::net_dev(s).into_iter().map(|i| i.name).collect())
            .unwrap_or_default();

        Facts {
            hostname: sysinfo::System::host_name().unwrap_or_else(|| "unknown".into()),
            os: sysinfo::System::long_os_version().unwrap_or_else(|| "Linux".into()),
            kernel: sysinfo::System::kernel_version(),
            arch: sysinfo::System::cpu_arch(),
            cpu_model: cpuinfo.as_deref().and_then(parse::cpu_model),
            cpu_cores: {
                let host = sysinfo::System::physical_core_count().unwrap_or(0) as u32;
                // 容器里取 cgroup 配额与宿主机核数的较小者，向上取整
                // （0.5 核仍然要显示成 1，不能显示成 0）
                match self.read_cgroup_cpu_quota() {
                    Some(q) if q > 0.0 => (q.ceil() as u32).min(host.max(1)),
                    _ => host,
                }
            },
            virtualization: self.detect_virtualization(cpuinfo.as_deref()),
            mem_total: mem.total,
            swap_total: mem.swap_total,
            disk_total: self.read_disk(&RuntimeConfig::default()).total,
            boot_at: stat.as_deref().and_then(parse::btime).unwrap_or(0),
            interfaces: ifaces,
            capabilities: self.caps.clone(),
        }
    }

    fn sample(&mut self, cfg: &RuntimeConfig) -> Metrics {
        // ── CPU ──
        let cur_cpu = self
            .roots
            .proc("stat")
            .as_deref()
            .and_then(parse::cpu_times);
        let cpu_pct = match (self.prev_cpu, cur_cpu) {
            (Some(p), Some(c)) => CpuTimes::usage_pct(p, c).unwrap_or(0.0),
            _ => 0.0,
        };
        if cur_cpu.is_some() {
            self.prev_cpu = cur_cpu;
        }

        // ── 网卡 ──
        let stats = self
            .roots
            .proc("net/dev")
            .as_deref()
            .map(parse::net_dev)
            .unwrap_or_default();
        let names: Vec<String> = stats.iter().map(|s| s.name.clone()).collect();
        let picked: Vec<String> = filter::select(&names, &cfg.net_include, &cfg.net_exclude)
            .into_iter()
            .cloned()
            .collect();
        let (rx, tx) = stats
            .iter()
            .filter(|s| picked.contains(&s.name))
            .fold((0u64, 0u64), |(a, b), s| (a + s.rx_bytes, b + s.tx_bytes));

        let now = Instant::now();
        let (rx_speed, tx_speed) = match self.prev_net.replace((rx, tx, now)) {
            Some((prx, ptx, pt)) => {
                let s = now.duration_since(pt).as_secs_f64();
                (rate(prx, rx, s), rate(ptx, tx, s))
            }
            None => (0, 0),
        };

        // ── 连接数 / 进程数 ──
        let sockstat = cfg
            .report_conn_count
            .then(|| self.roots.proc("net/sockstat"))
            .flatten();
        let tcp_conn = sockstat
            .as_deref()
            .and_then(|s| parse::sockstat_inuse(s, "TCP:"));
        let udp_conn = sockstat
            .as_deref()
            .and_then(|s| parse::sockstat_inuse(s, "UDP:"));

        // hidepid 下读到的是「一个很小的错数」而不是「读不到」，
        // 所以按能力声明直接不上报，而不是上报一个错的
        let proc_count = self
            .caps
            .proc_count
            .then(|| count_processes(&self.roots.proc));

        let load = self
            .roots
            .proc("loadavg")
            .as_deref()
            .and_then(parse::loadavg)
            .map(|l| l.map(load_fixed))
            .unwrap_or([0; 3]);

        Metrics {
            ts: crate::now_unix(),
            cpu_pct: pulse_proto::pct_to_basis_points(cpu_pct),
            load,
            mem: self.read_mem(),
            disk: self.read_disk(cfg),
            net: NetStat {
                rx_bytes: rx,
                tx_bytes: tx,
                rx_speed,
                tx_speed,
                ifaces: picked,
            },
            tcp_conn,
            udp_conn,
            proc_count,
            gpu: (cfg.gpu_enabled && self.caps.gpu_nvml)
                .then(|| self.gpu.sample())
                .flatten(),
            cpu_temp: cfg.report_temps.then(|| self.read_temp()).flatten(),
            uptime_s: self
                .roots
                .proc("uptime")
                .as_deref()
                .and_then(parse::uptime_s)
                .unwrap_or(0),
        }
    }
}

impl LinuxCollector {
    fn detect_virtualization(&self, cpuinfo: Option<&str>) -> Option<String> {
        // 容器优先：cgroup 路径里能看出来
        if let Some(c) = self.roots.proc("1/cgroup") {
            for (needle, name) in [
                ("docker", "docker"),
                ("kubepods", "kubernetes"),
                ("lxc", "lxc"),
                ("containerd", "containerd"),
            ] {
                if c.contains(needle) {
                    return Some(name.into());
                }
            }
        }
        // DMI 给出具体的虚拟化厂商。product_name / sys_vendor 是 0444；
        // product_serial / product_uuid 是 0400 —— 那些是机器指纹，我们不读。
        //
        // **两个都要看**：很多 KVM 机器的 product_name 是
        // "Standard PC (Q35 + ICH9, 2009)"，一点厂商信息都没有，
        // 但 sys_vendor 明明白白写着 QEMU。只查 product_name 的话
        // 这类机器会一路掉到最后的 CPUID 分支，报一个没信息量的 "vm"。
        for key in ["class/dmi/id/product_name", "class/dmi/id/sys_vendor"] {
            let Some(p) = self.sys_trim(key) else {
                continue;
            };
            let lower = p.to_lowercase();
            for (needle, name) in [
                ("kvm", "kvm"),
                ("vmware", "vmware"),
                ("virtualbox", "virtualbox"),
                ("bochs", "kvm"),
                // QEMU 后端绝大多数是 KVM 加速；纯 TCG 软件模拟极少见，
                // 且两者对用户的意义相同，不值得为区分它去读 CPUID 厂商串
                ("qemu", "kvm"),
                ("hyper-v", "hyperv"),
                ("microsoft corporation", "hyperv"),
                ("virtual machine", "hyperv"),
                ("droplet", "kvm"),
                ("openstack", "kvm"),
                ("xen", "xen"),
                ("amazon ec2", "xen"),
                ("google", "gce"),
                ("alibaba", "kvm"),
            ] {
                if lower.contains(needle) {
                    return Some(name.into());
                }
            }
        }
        // aarch64 常无 DMI，回落到 CPUID 的 hypervisor 位（仅 x86 有意义）
        match cpuinfo.map(parse::has_hypervisor_flag) {
            Some(true) => Some("vm".into()),
            _ => Some("none".into()),
        }
    }

    fn sys_trim(&self, rel: &str) -> Option<String> {
        self.roots
            .sys(rel)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

/// 当前进程的全部 gid（含附加组），用于判断非特权 ICMP 是否可用。
fn current_gids() -> Vec<u32> {
    // SAFETY: getgid/getegid 无参数、无副作用、不会失败
    let mut v = unsafe { vec![libc::getgid(), libc::getegid()] };
    // SAFETY: 传 0 与空指针是 getgroups 的标准用法，表示「只查数量不写数据」
    let n = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if n > 0 {
        let mut buf = vec![0 as libc::gid_t; n as usize];
        // SAFETY: buf 长度与传入的 n 一致
        if unsafe { libc::getgroups(n, buf.as_mut_ptr()) } > 0 {
            v.extend(buf);
        }
    }
    v.sort_unstable();
    v.dedup();
    v
}

/// 数 `/proc` 下的数字目录。调用方须先确认 `capabilities.proc_count`。
fn count_processes(proc_root: &Path) -> u32 {
    std::fs::read_dir(proc_root)
        .map(|d| {
            d.flatten()
                .filter(|e| {
                    e.file_name().to_str().is_some_and(|n| {
                        n.as_bytes().iter().all(u8::is_ascii_digit) && !n.is_empty()
                    })
                })
                .count() as u32
        })
        .unwrap_or(0)
}

/// 返回 `(总字节, 可用字节)`。路径不可搜索时返回 `None`。
///
/// ⚠️ `ProtectHome=yes` 会让 `/home` `/root` 变得 inaccessible，
/// 挂在那底下的数据盘会走到这个 `None` 分支 —— unit 里必须用 `read-only`
/// 。
fn statvfs(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs 是纯 POD 结构（全是整数字段），全零是合法初始状态；
    // 随后由内核完整写入
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: c 是有效的 NUL 结尾字符串，st 是合法的可写 statvfs
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let bs = if st.f_frsize > 0 {
        st.f_frsize
    } else {
        st.f_bsize
    } as u64;
    Some(disk_usage(
        st.f_blocks as u64,
        st.f_bfree as u64,
        st.f_bavail as u64,
        bs,
    ))
}

/// 从 statvfs 的原始块数算出 `(总量, 已用)`，**口径与 `df` 一致**。
///
/// ext4 默认给 root 预留 5% 的块（`f_bfree - f_bavail`）。
/// 早先这里用 `总量 - f_bavail` 当已用，于是把预留块也算成了「已用」——
/// 真机实测：`df` 说已用 1.4 GB / 3%，面板说 4.0 GB / 6.4%，
/// 差 2.8 倍。用户一对照就会认为面板坏了。
///
/// 现在的口径：
/// - **已用** = `f_blocks - f_bfree`，与 `df` 的 Used 列逐字节相同
/// - **总量** = 已用 + `f_bavail`，也就是「普通用户看得到的容量」，
///   于是 `已用 / 总量` 与 `df` 的 Use% 相同
///
/// 代价是总量比 `df` 的 Size 列小一个预留量（本机 63.3 → 60.7 GB）。
/// 这是刻意的取舍：用户真正会盯着的是「用了多少」和「满了没有」，
/// 而且按这个口径，`df` 显示 100% 时面板也显示 100% —— 用
/// `f_blocks` 当分母的话，磁盘真满了面板只会显示 95%，告警阈值形同虚设。
fn disk_usage(blocks: u64, bfree: u64, bavail: u64, bs: u64) -> (u64, u64) {
    let used = blocks.saturating_sub(bfree);
    let total = used.saturating_add(bavail);
    (total.saturating_mul(bs), used.saturating_mul(bs))
}

#[cfg(test)]
mod tests {

    /// 真机取到的一组 statvfs 原始值（Debian 13 / ext4 / 59 GiB 根分区）。
    /// 同一时刻 `df -B1 /` 输出 Used=1434800128、Use%=3%。
    #[test]
    fn disk_usage_matches_df_on_a_real_ext4() {
        let bs = 4096;
        let (blocks, bfree, bavail) = (15_444_671, 15_094_378, 14_458_083);
        let (total, used) = disk_usage(blocks, bfree, bavail, bs);
        assert_eq!(used, 1_434_800_128, "已用必须与 df 的 Used 列逐字节相同");
        let pct = used as f64 / total as f64 * 100.0;
        assert!(
            (pct - 2.37).abs() < 0.02,
            "Use% 应当与 df 一致（未取整前 2.37%，df 显示 3%），实际 {pct:.2}"
        );
        // 旧口径会把 root 预留块也算成已用，差 2.8 倍 —— 这是这个测试要挡住的回归
        let old = (blocks - bavail) * bs;
        assert!(old > used * 2, "旧口径 {old} 应当明显大于 df 的 {used}");
    }

    /// 关键性质：`df` 显示 100% 时面板也必须显示 100%。
    ///
    /// 用 `f_blocks` 当分母的话，磁盘真满了（f_bavail=0）面板只会显示 95%，
    /// 95% 的告警阈值就永远差一口气 —— 这正是旧口径的危险之处。
    #[test]
    fn full_disk_reads_as_one_hundred_percent() {
        let bs = 4096;
        // 预留 5%，可用为 0：df 此时显示 100%
        let (blocks, bfree, bavail) = (1_000_000, 50_000, 0);
        let (total, used) = disk_usage(blocks, bfree, bavail, bs);
        assert_eq!(total, used, "可用为 0 时已用必须等于总量");
        assert_eq!(used as f64 / total as f64 * 100.0, 100.0);
    }

    #[test]
    fn disk_usage_handles_degenerate_values_without_panicking() {
        // 空文件系统、以及 bfree > blocks 这种坏数据
        assert_eq!(disk_usage(0, 0, 0, 4096), (0, 0));
        let (t, u) = disk_usage(100, 999, 0, 4096);
        assert_eq!(
            (t, u),
            (0, 0),
            "bfree 大于 blocks 时应当饱和到 0 而不是回绕"
        );
    }
    use super::*;
    use std::fs;

    /// 搭一个假的 `/proc` + `/sys` 树。
    ///
    /// 这让「哪个文件填哪个字段」的组装逻辑可以在任何开发机上验到 L4，
    /// 真机上就只剩一个问题：这些文件在非 root 下读不读得到
    /// （那个问题由 `tools/check-linux-caps.sh` 回答）。
    struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let f = Self {
                dir: tempfile::tempdir().unwrap(),
            };
            f.write("proc/stat", PROC_STAT_1);
            f.write("proc/meminfo", MEMINFO);
            f.write("proc/loadavg", "0.52 0.58 0.59 2/1234 5678\n");
            f.write("proc/uptime", "123456.78 987654.32\n");
            f.write("proc/net/dev", NET_DEV);
            f.write("proc/net/sockstat", SOCKSTAT);
            f.write("proc/mounts", MOUNTS);
            f.write("proc/cpuinfo", CPUINFO);
            f.write("proc/sys/net/ipv4/ping_group_range", "0 2147483647\n");
            fs::create_dir_all(f.dir.path().join("proc/1")).unwrap();
            f
        }

        fn write(&self, rel: &str, content: &str) {
            let p = self.dir.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }

        fn remove(&self, rel: &str) {
            let _ = fs::remove_file(self.dir.path().join(rel));
            let _ = fs::remove_dir_all(self.dir.path().join(rel));
        }

        fn roots(&self) -> Roots {
            Roots {
                proc: self.dir.path().join("proc"),
                sys: self.dir.path().join("sys"),
                rootfs: PathBuf::new(),
            }
        }

        fn collector(&self) -> LinuxCollector {
            LinuxCollector::with_roots(self.roots())
        }
    }

    const PROC_STAT_1: &str = "cpu  1000 0 0 9000 0 0 0 0 0 0\nbtime 1756000000\n";
    // 相对上一次：总量 +1000，其中 idle +800 → 忙 20%
    const PROC_STAT_2: &str = "cpu  1200 0 0 9800 0 0 0 0 0 0\nbtime 1756000000\n";
    const MEMINFO: &str = "\
MemTotal:        4030264 kB
MemFree:          182364 kB
MemAvailable:    2938104 kB
Buffers:          104856 kB
Cached:          2721920 kB
SReclaimable:     186532 kB
SwapTotal:       2097148 kB
SwapFree:        2097148 kB
";
    const NET_DEV: &str = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 1000000    100    0    0    0     0          0         0  1000000     100    0    0    0     0       0          0
  eth0: 5000000    500    0    0    0     0          0         0  3000000     300    0    0    0     0       0          0
docker0:  700000     70    0    0    0     0          0         0   200000      20    0    0    0     0       0          0
";
    const SOCKSTAT: &str = "\
sockets: used 234
TCP: inuse 42 orphan 0 tw 3 alloc 20 mem 2
UDP: inuse 7 mem 1
";
    const MOUNTS: &str = "\
sysfs /sys sysfs rw,nosuid,nodev,noexec,relatime 0 0
proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0
/dev/vda1 / ext4 rw,relatime 0 0
tmpfs /run tmpfs rw,nosuid,nodev,size=402424k 0 0
overlay /var/lib/docker/overlay2/abc/merged overlay rw,relatime 0 0
";
    const CPUINFO: &str = "\
processor\t: 0
model name\t: Intel(R) Xeon(R) Platinum 8375C CPU @ 2.90GHz
flags\t\t: fpu vme de pse tsc msr hypervisor lahf_lm
";

    #[test]
    fn assembles_all_fields_from_proc() {
        let f = Fixture::new();
        let mut c = f.collector();
        let m = c.sample(&RuntimeConfig::default());

        // 内存：kB → 字节
        assert_eq!(m.mem.total, 4030264 * 1024);
        assert_eq!(m.mem.available, 2938104 * 1024);
        assert_eq!(m.mem.swap_total, 2097148 * 1024);
        // 负载 ×100
        assert_eq!(m.load, [52, 58, 59]);
        assert_eq!(m.uptime_s, 123456);
        assert_eq!(m.tcp_conn, Some(42));
        assert_eq!(m.udp_conn, Some(7));
        assert!(m.proc_count.is_some());
    }

    #[test]
    fn cpu_percent_comes_from_delta_between_samples() {
        let f = Fixture::new();
        let mut c = f.collector(); // 构造时已用 PROC_STAT_1 预热

        f.write("proc/stat", PROC_STAT_2);
        let m = c.sample(&RuntimeConfig::default());
        // 总量 +1000，idle +800 → 忙 20% → 万分比 2000
        assert_eq!(m.cpu_pct, 2000, "CPU 必须由两次采样的差值算出");
    }

    #[test]
    fn default_filters_exclude_loopback_and_docker() {
        let f = Fixture::new();
        let mut c = f.collector();
        let m = c.sample(&RuntimeConfig::default());

        assert_eq!(
            m.net.ifaces,
            vec!["eth0"],
            "lo 与 docker0 必须被默认黑名单排除"
        );
        assert_eq!(m.net.rx_bytes, 5_000_000, "只能统计 eth0，不含 lo/docker0");
        assert_eq!(m.net.tx_bytes, 3_000_000);
    }

    #[test]
    fn net_include_overrides_defaults() {
        let f = Fixture::new();
        let mut c = f.collector();
        let cfg = RuntimeConfig {
            net_include: vec!["lo".into(), "eth0".into()],
            net_exclude: vec![],
            ..Default::default()
        };
        let m = c.sample(&cfg);
        assert_eq!(m.net.ifaces, vec!["lo", "eth0"]);
        assert_eq!(m.net.rx_bytes, 6_000_000);
    }

    #[test]
    fn first_sample_reports_zero_speed_not_garbage() {
        // 没有上一次采样时不能拿累计值当速率报出去
        let f = Fixture::new();
        let mut c = f.collector();
        let m = c.sample(&RuntimeConfig::default());
        assert_eq!((m.net.rx_speed, m.net.tx_speed), (0, 0));
    }

    #[test]
    fn hidepid_marks_process_count_untrusted_instead_of_reporting_wrong_number() {
        // hidepid 下非 root 只看得到自己的进程 —— 读到的是「一个很小的错数」，
        // 必须显式标记不可信，而不是照样上报
        let f = Fixture::new();
        f.write(
            "proc/mounts",
            &MOUNTS.replace(
                "proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0",
                "proc /proc proc rw,nosuid,nodev,noexec,relatime,hidepid=2 0 0",
            ),
        );
        let mut c = f.collector();
        let facts = c.facts();
        assert!(
            !facts.capabilities.proc_count,
            "hidepid 下 proc_count 能力必须为 false"
        );
        assert_eq!(
            c.sample(&RuntimeConfig::default()).proc_count,
            None,
            "不能上报错数"
        );
    }

    #[test]
    fn missing_pid_one_also_marks_process_count_untrusted() {
        // ProtectProc=invisible 的情况：看不到 PID 1
        let f = Fixture::new();
        f.remove("proc/1");
        let mut c = f.collector();
        assert!(!c.facts().capabilities.proc_count);
    }

    #[test]
    fn container_without_lxcfs_uses_cgroup_limit_not_host_memory() {
        // 这是便宜 VPS 上最常见的坑：LXC/OpenVZ 未挂 lxcfs 时 /proc/meminfo
        // 显示的是**宿主机**内存，一台 1 GB 的小鸡会显示宿主机的 128 GB。
        let f = Fixture::new();
        f.write("sys/fs/cgroup/memory.max", "1073741824\n"); // 1 GiB
        let mut c = f.collector();

        let facts = c.facts();
        assert!(
            facts.capabilities.cgroup_limited,
            "必须标记出「规格来自 cgroup」"
        );
        assert_eq!(
            facts.mem_total,
            1 << 30,
            "应取 cgroup 限额而不是 /proc 的宿主机数值"
        );

        let m = c.sample(&RuntimeConfig::default());
        assert_eq!(m.mem.total, 1 << 30);
        assert!(m.mem.available <= m.mem.total, "可用不能超过总量");
        assert!(m.mem.free <= m.mem.total);
    }

    #[test]
    fn cgroup_unlimited_falls_back_to_proc() {
        let f = Fixture::new();
        f.write("sys/fs/cgroup/memory.max", "max\n");
        let mut c = f.collector();
        let facts = c.facts();
        assert!(!facts.capabilities.cgroup_limited);
        assert_eq!(facts.mem_total, 4030264 * 1024);
    }

    #[test]
    fn lxcfs_present_means_proc_is_already_container_scoped() {
        let f = Fixture::new();
        f.write("sys/fs/cgroup/memory.max", "1073741824\n");
        f.write(
            "proc/mounts",
            &format!("{MOUNTS}lxcfs /proc/meminfo fuse.lxcfs rw 0 0\n"),
        );
        let mut c = f.collector();
        // 挂了 lxcfs 说明 /proc 已经是容器视角，不必再提示用户
        assert!(!c.facts().capabilities.cgroup_limited);
    }

    #[test]
    fn missing_temperature_sensors_report_none_not_zero() {
        // 多数 KVM / 容器根本没有传感器 —— 这与权限无关。
        // 报 0°C 会在前端画出一条假的零线。
        let f = Fixture::new();
        let mut c = f.collector();
        assert!(!c.facts().capabilities.temperature);
        assert_eq!(c.sample(&RuntimeConfig::default()).cpu_temp, None);
    }

    #[test]
    fn temperature_picks_the_hottest_sensor() {
        let f = Fixture::new();
        f.write("sys/class/hwmon/hwmon0/temp1_input", "42000\n");
        f.write("sys/class/hwmon/hwmon1/temp1_input", "58000\n");
        f.write("sys/class/hwmon/hwmon1/temp2_input", "999000\n"); // 坏传感器，应丢弃
        let mut c = f.collector();
        assert!(c.facts().capabilities.temperature);
        assert_eq!(c.sample(&RuntimeConfig::default()).cpu_temp, Some(580));
    }

    #[test]
    fn icmp_capability_follows_ping_group_range() {
        let f = Fixture::new();
        assert!(f.collector().facts().capabilities.icmp_unprivileged);

        // 内核默认 "1 0" 是空区间 —— 禁用，延迟监控要回落 TCP
        f.write("proc/sys/net/ipv4/ping_group_range", "1 0\n");
        assert!(!f.collector().facts().capabilities.icmp_unprivileged);
    }

    #[test]
    fn detects_virtualization_from_dmi_then_cpuid() {
        let f = Fixture::new();
        // 无 DMI（aarch64 常见）→ 回落 CPUID 的 hypervisor 位
        assert_eq!(f.collector().facts().virtualization.as_deref(), Some("vm"));

        f.write("sys/class/dmi/id/product_name", "KVM\n");
        assert_eq!(f.collector().facts().virtualization.as_deref(), Some("kvm"));

        f.write("sys/class/dmi/id/product_name", "VMware Virtual Platform\n");
        assert_eq!(
            f.collector().facts().virtualization.as_deref(),
            Some("vmware")
        );

        // 回归：真机上 product_name 是 "Standard PC (Q35 + ICH9, 2009)"，
        // 一点厂商信息都没有，但 sys_vendor 写着 QEMU。
        // 只查 product_name 的话会掉到 CPUID 分支报一个没信息量的 "vm"。
        f.write(
            "sys/class/dmi/id/product_name",
            "Standard PC (Q35 + ICH9, 2009)\n",
        );
        f.write("sys/class/dmi/id/sys_vendor", "QEMU\n");
        assert_eq!(f.collector().facts().virtualization.as_deref(), Some("kvm"));

        // 容器优先于 DMI
        f.write("proc/1/cgroup", "0::/docker/abc123\n");
        assert_eq!(
            f.collector().facts().virtualization.as_deref(),
            Some("docker")
        );
    }

    #[test]
    fn bare_metal_reports_none() {
        let f = Fixture::new();
        f.write("proc/cpuinfo", &CPUINFO.replace(" hypervisor", ""));
        assert_eq!(
            f.collector().facts().virtualization.as_deref(),
            Some("none")
        );
    }

    #[test]
    fn disk_only_counts_real_filesystems() {
        // rootfs 指向 fixture 目录，让 "/" 这个挂载点能真的 statvfs 成功
        let f = Fixture::new();
        let mut roots = f.roots();
        roots.rootfs = f.dir.path().to_path_buf();
        let mut c = LinuxCollector::with_roots(roots);

        let m = c.sample(&RuntimeConfig::default());
        // fixture 的 mounts 里只有 /dev/vda1 是 ext4；
        // tmpfs / overlay / proc / sysfs 都不该计入，否则磁盘占用虚高
        assert!(m.disk.total > 0, "应统计到 ext4 挂载点");
        assert!(m.disk.used <= m.disk.total);
    }

    #[test]
    fn survives_a_completely_empty_proc() {
        // 对抗性自检：所有文件都读不到时不能 panic，只能全部降级
        let dir = tempfile::tempdir().unwrap();
        let mut c = LinuxCollector::with_roots(Roots {
            proc: dir.path().join("nonexistent"),
            sys: dir.path().join("nonexistent"),
            rootfs: PathBuf::new(),
        });
        let facts = c.facts();
        assert!(!facts.capabilities.proc_count);
        assert!(!facts.capabilities.tcp_conn_count);

        let m = c.sample(&RuntimeConfig::default());
        assert_eq!(m.cpu_pct, 0);
        assert_eq!(m.mem.total, 0);
        assert_eq!(m.tcp_conn, None);
        assert!(m.net.ifaces.is_empty());
    }

    #[test]
    fn survives_garbage_in_every_file() {
        let f = Fixture::new();
        for p in [
            "proc/stat",
            "proc/meminfo",
            "proc/loadavg",
            "proc/uptime",
            "proc/net/dev",
            "proc/net/sockstat",
            "proc/mounts",
            "proc/cpuinfo",
        ] {
            f.write(p, "\u{0}garbage\n\n!!!not a number!!!\n");
        }
        let mut c = f.collector();
        let m = c.sample(&RuntimeConfig::default()); // 不能 panic
        assert_eq!(m.cpu_pct, 0);
        assert_eq!(m.load, [0; 3]);
    }

    #[test]
    fn report_toggles_are_honoured() {
        let f = Fixture::new();
        f.write("sys/class/hwmon/hwmon0/temp1_input", "42000\n");
        let mut c = f.collector();

        let off = RuntimeConfig {
            report_temps: false,
            report_conn_count: false,
            ..Default::default()
        };
        let m = c.sample(&off);
        assert_eq!(m.cpu_temp, None, "关闭后不应采集温度");
        assert_eq!(m.tcp_conn, None, "关闭后不应采集连接数");
    }
}
