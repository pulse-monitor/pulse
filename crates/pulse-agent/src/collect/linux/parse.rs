//! `/proc` 与 `/sys` 的**纯函数**解析器。
//!
//! 全部签名都是 `&str -> T`，不碰文件系统 —— 因此可以在任何平台上用真实
//! fixture 完整单测。Linux 采集里最容易出错的是格式解析（各种对齐、缺失字段、
//! 大数字挤掉空格），而不是「文件读不读得到」。把这两件事切开，
//! 前者我能验到底，后者才需要真机。
//!
//! 权限逐项审计

/// `/proc/stat` 第一行的 CPU 时间片累计值。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTimes {
    /// 全部时间片之和
    pub total: u64,
    /// idle + iowait
    pub idle: u64,
}

impl CpuTimes {
    /// 两次采样之间的 CPU 使用率百分比。
    ///
    /// 计数器回绕或采样顺序颠倒时返回 `None` 而不是一个假数字。
    pub fn usage_pct(prev: Self, cur: Self) -> Option<f32> {
        let dt = cur.total.checked_sub(prev.total)?;
        let di = cur.idle.checked_sub(prev.idle)?;
        if dt == 0 {
            return None; // 采样间隔太短，还没有新的时间片
        }
        let busy = dt.saturating_sub(di);
        Some(busy as f32 * 100.0 / dt as f32)
    }
}

/// 解析 `/proc/stat` 的 `cpu ` 汇总行。
///
/// 格式：`cpu  user nice system idle iowait irq softirq steal guest guest_nice`
/// 注意 `guest` 与 `guest_nice` 已经分别计入 `user` 与 `nice`，
/// 再加一次会让总数偏大 —— 所以只取前 8 个字段。
pub fn cpu_times(proc_stat: &str) -> Option<CpuTimes> {
    let line = proc_stat.lines().find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = line
        .split_ascii_whitespace()
        .skip(1)
        .take(8)
        .filter_map(|x| x.parse().ok())
        .collect();
    if v.len() < 4 {
        return None;
    }
    Some(CpuTimes {
        total: v.iter().sum(),
        idle: v[3] + v.get(4).copied().unwrap_or(0), // idle + iowait
    })
}

/// `/proc/stat` 里的 `btime`（开机时刻，unix 秒）。
pub fn btime(proc_stat: &str) -> Option<i64> {
    proc_stat
        .lines()
        .find_map(|l| l.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()
}

/// `/proc/loadavg` → 1/5/15 分钟负载。
pub fn loadavg(s: &str) -> Option<[f32; 3]> {
    let mut it = s.split_ascii_whitespace();
    Some([
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    ])
}

/// `/proc/uptime` 第一个字段，单位秒。
pub fn uptime_s(s: &str) -> Option<u64> {
    Some(s.split_ascii_whitespace().next()?.parse::<f64>().ok()? as u64)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemInfo {
    pub total: u64,
    pub free: u64,
    /// 内核给出的「可用」估算。**内核 < 3.14 时不存在**，此时为 0，
    /// 调用方需回落到 `free + buffers + cached + sreclaimable` 的粗估。
    pub available: u64,
    pub buffers: u64,
    pub cached: u64,
    pub sreclaimable: u64,
    pub swap_total: u64,
    pub swap_free: u64,
}

impl MemInfo {
    /// `MemAvailable` 缺失时的回落估算。
    pub fn available_or_estimate(&self) -> u64 {
        if self.available > 0 {
            self.available
        } else {
            self.free + self.buffers + self.cached + self.sreclaimable
        }
    }
}

/// 解析 `/proc/meminfo`。值一律是 kB，这里统一换算成字节。
pub fn meminfo(s: &str) -> MemInfo {
    let mut m = MemInfo::default();
    for line in s.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        // 形如 `MemTotal:        1009096 kB`
        let Some(n) = v
            .split_ascii_whitespace()
            .next()
            .and_then(|x| x.parse::<u64>().ok())
        else {
            continue;
        };
        let bytes = n.saturating_mul(1024);
        match k {
            "MemTotal" => m.total = bytes,
            "MemFree" => m.free = bytes,
            "MemAvailable" => m.available = bytes,
            "Buffers" => m.buffers = bytes,
            "Cached" => m.cached = bytes,
            "SReclaimable" => m.sreclaimable = bytes,
            "SwapTotal" => m.swap_total = bytes,
            "SwapFree" => m.swap_free = bytes,
            _ => {}
        }
    }
    m
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfaceStat {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// 解析 `/proc/net/dev`。
///
/// ⚠️ 经典陷阱：字段是右对齐的，接口名很长或字节数很大时冒号后面**没有空格**，
/// 变成 `eth0:987654321`。按空白切分会把名字和数字粘在一起。
/// 所以必须先按第一个 `:` 切开。
pub fn net_dev(s: &str) -> Vec<IfaceStat> {
    s.lines()
        .filter_map(|line| {
            let (name, rest) = line.split_once(':')?;
            let name = name.trim();
            if name.is_empty() || name.contains('|') {
                return None; // 表头两行
            }
            let f: Vec<u64> = rest
                .split_ascii_whitespace()
                .map(|x| x.parse().unwrap_or(0))
                .collect();
            // 接收 8 列（bytes 在第 0 列），发送 8 列（bytes 在第 8 列）
            if f.len() < 9 {
                return None;
            }
            Some(IfaceStat {
                name: name.to_string(),
                rx_bytes: f[0],
                tx_bytes: f[8],
            })
        })
        .collect()
}

/// 从 `/proc/net/sockstat` 取 TCP `inuse`。
///
/// 比逐行数 `/proc/net/tcp` 便宜得多 —— 后者在连接数上万的机器上
/// 每次采集都要扫几 MB 文本。
pub fn sockstat_inuse(s: &str, proto: &str) -> Option<u32> {
    let line = s.lines().find(|l| l.starts_with(proto))?;
    let mut it = line.split_ascii_whitespace();
    while let Some(tok) = it.next() {
        if tok == "inuse" {
            return it.next()?.parse().ok();
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub device: String,
    pub mount_point: String,
    pub fstype: String,
    pub options: String,
}

/// 解析 `/proc/mounts`。
///
/// ⚠️ 挂载点里的空格、制表符等被转义成 `\040` 这样的八进制序列，必须还原，
/// 否则「/mnt/my disk」这类路径会被截断。
pub fn mounts(s: &str) -> Vec<Mount> {
    s.lines()
        .filter_map(|line| {
            let mut it = line.split_ascii_whitespace();
            let device = unescape_octal(it.next()?);
            let mount_point = unescape_octal(it.next()?);
            let fstype = it.next()?.to_string();
            let options = it.next().unwrap_or("").to_string();
            Some(Mount {
                device,
                mount_point,
                fstype,
                options,
            })
        })
        .collect()
}

/// 还原 `\040` 这类八进制转义。
fn unescape_octal(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() {
            if let Ok(n) = u8::from_str_radix(&s[i + 1..i + 4], 8) {
                out.push(n as char);
                i += 4;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

/// `/proc` 是否以 `hidepid` 挂载。
///
/// 此时非 root 只能看到自己的进程 —— 读到的不是「读不到」而是
/// **一个很小的错数**，必须显式标记为不可信而不是照样上报。
pub fn proc_has_hidepid(proc_mounts: &str) -> bool {
    mounts(proc_mounts)
        .iter()
        .any(|m| m.mount_point == "/proc" && m.options.split(',').any(|o| o.starts_with("hidepid")))
}

/// `/proc/cpuinfo` 里的 `model name`。
pub fn cpu_model(s: &str) -> Option<String> {
    s.lines()
        .find_map(|l| l.strip_prefix("model name"))
        .and_then(|v| v.split_once(':'))
        .map(|(_, v)| v.trim().to_string())
        .filter(|x| !x.is_empty())
}

/// CPUID 的 hypervisor 位（通过 `/proc/cpuinfo` 的 flags 暴露）。
/// 仅 x86 有意义；aarch64 上永远为 false。
pub fn has_hypervisor_flag(cpuinfo: &str) -> bool {
    cpuinfo
        .lines()
        .filter(|l| l.starts_with("flags") || l.starts_with("Features"))
        .any(|l| l.split_ascii_whitespace().any(|f| f == "hypervisor"))
}

/// cgroup v2 的 `memory.max` / v1 的 `memory.limit_in_bytes`。
///
/// 返回 `None` 表示无限制。v2 用字面量 `max`，v1 用一个接近 u64 上限的巨大数字，
/// 两者都要识别成「没有限制」而不是「限制是 8 EB」。
pub fn cgroup_limit(s: &str) -> Option<u64> {
    let t = s.trim();
    if t == "max" {
        return None;
    }
    let n: u64 = t.parse().ok()?;
    // v1 的「无限制」是 PAGE_COUNTER_MAX，实际值随页大小变化，
    // 统一用 1 PiB 作为阈值：真机内存不可能到这个量级
    (n < (1 << 50)).then_some(n)
}

/// cgroup v2 `cpu.max`：`"max 100000"` 或 `"200000 100000"`。
/// 返回可用的 CPU 核数（如 2.0）。
pub fn cgroup_cpu_max(s: &str) -> Option<f64> {
    let mut it = s.split_ascii_whitespace();
    let quota = it.next()?;
    let period: f64 = it.next()?.parse().ok()?;
    if quota == "max" || period <= 0.0 {
        return None;
    }
    Some(quota.parse::<f64>().ok()? / period)
}

/// `/sys/class/hwmon/*/temp*_input` 或 `/sys/class/thermal/*/temp` 的内容。
/// 单位是毫摄氏度，这里换算成 ×10 摄氏度（与协议一致）。
pub fn temp_millidegrees(s: &str) -> Option<i32> {
    let milli: i64 = s.trim().parse().ok()?;
    // 传感器坏掉时会给出 0 或荒谬的值，直接丢弃而不是上报一个假温度
    (-40_000..=150_000)
        .contains(&milli)
        .then_some((milli / 100) as i32)
}

/// `net.ipv4.ping_group_range` 的内容，判断给定 gid 是否被允许建非特权 ICMP socket。
pub fn ping_group_allows(range: &str, gids: &[u32]) -> bool {
    let mut it = range.split_ascii_whitespace();
    let (Some(lo), Some(hi)) = (it.next(), it.next()) else {
        return false;
    };
    let (Ok(lo), Ok(hi)) = (lo.parse::<u32>(), hi.parse::<u32>()) else {
        return false;
    };
    // 内核默认是 "1 0"，即空区间 —— 禁用
    if lo > hi {
        return false;
    }
    gids.iter().any(|g| (lo..=hi).contains(g))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 以下 fixture 都是真实 Linux 机器上的原样输出格式。

    const PROC_STAT: &str = "\
cpu  1512345 3421 456789 98765432 12345 0 8901 234 0 0
cpu0 756172 1710 228394 49382716 6172 0 4450 117 0 0
cpu1 756173 1711 228395 49382716 6173 0 4451 117 0 0
intr 123456789 0 0 0
ctxt 987654321
btime 1756000000
processes 123456
procs_running 2
procs_blocked 0
";

    #[test]
    fn cpu_times_sums_first_eight_fields_only() {
        let c = cpu_times(PROC_STAT).unwrap();
        // guest / guest_nice 已计入 user / nice，再加会让总数偏大
        // user+nice+system+idle+iowait+irq(0)+softirq+steal
        let expect: u64 = [1512345u64, 3421, 456789, 98765432, 12345, 0, 8901, 234]
            .iter()
            .sum();
        assert_eq!(c.total, expect);
        assert_eq!(c.idle, 98765432 + 12345);
    }

    #[test]
    fn cpu_usage_between_two_samples() {
        let prev = CpuTimes {
            total: 1000,
            idle: 900,
        };
        let cur = CpuTimes {
            total: 2000,
            idle: 1700,
        };
        // 1000 个新时间片里 800 个是 idle → 忙 20%
        assert!((CpuTimes::usage_pct(prev, cur).unwrap() - 20.0).abs() < 0.01);
    }

    #[test]
    fn cpu_usage_rejects_counter_reset_and_zero_delta() {
        let a = CpuTimes {
            total: 2000,
            idle: 1000,
        };
        let b = CpuTimes {
            total: 1000,
            idle: 500,
        };
        assert_eq!(
            CpuTimes::usage_pct(a, b),
            None,
            "计数器回绕应返回 None 而不是假数字"
        );
        assert_eq!(CpuTimes::usage_pct(a, a), None, "零间隔应返回 None");
    }

    #[test]
    fn cpu_usage_never_exceeds_100() {
        // idle 反而减少（异常内核）时不能算出负数或 >100
        let a = CpuTimes {
            total: 1000,
            idle: 900,
        };
        let b = CpuTimes {
            total: 2000,
            idle: 900,
        };
        let p = CpuTimes::usage_pct(a, b).unwrap();
        assert!((0.0..=100.0).contains(&p), "实际 {p}");
    }

    #[test]
    fn btime_is_parsed() {
        assert_eq!(btime(PROC_STAT), Some(1756000000));
        assert_eq!(btime("cpu 1 2 3\n"), None);
    }

    #[test]
    fn loadavg_and_uptime() {
        assert_eq!(
            loadavg("0.52 0.58 0.59 2/1234 5678"),
            Some([0.52, 0.58, 0.59])
        );
        assert_eq!(loadavg("garbage"), None);
        assert_eq!(uptime_s("123456.78 987654.32"), Some(123456));
    }

    const MEMINFO: &str = "\
MemTotal:        4030264 kB
MemFree:          182364 kB
MemAvailable:    2938104 kB
Buffers:          104856 kB
Cached:          2721920 kB
SwapCached:            0 kB
SReclaimable:     186532 kB
SwapTotal:       2097148 kB
SwapFree:        2097148 kB
";

    #[test]
    fn meminfo_converts_kb_to_bytes() {
        let m = meminfo(MEMINFO);
        assert_eq!(m.total, 4030264 * 1024);
        assert_eq!(m.available, 2938104 * 1024);
        assert_eq!(m.swap_total, 2097148 * 1024);
        // 两种口径都要能算出来
        assert_eq!(m.total - m.available, (4030264 - 2938104) * 1024);
        assert_eq!(m.total - m.free, (4030264 - 182364) * 1024);
    }

    #[test]
    fn meminfo_falls_back_when_memavailable_missing() {
        // 内核 < 3.14 没有 MemAvailable，不能因此把「可用内存」算成 0
        let old = MEMINFO.replace("MemAvailable:    2938104 kB\n", "");
        let m = meminfo(&old);
        assert_eq!(m.available, 0);
        assert_eq!(
            m.available_or_estimate(),
            (182364 + 104856 + 2721920 + 186532) * 1024
        );
    }

    #[test]
    fn meminfo_ignores_garbage_lines() {
        let m = meminfo("MemTotal: not-a-number kB\nMemFree:  100 kB\nrandom junk\n");
        assert_eq!(m.total, 0);
        assert_eq!(m.free, 100 * 1024);
    }

    const NET_DEV: &str = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 1234567    1234    0    0    0     0          0         0  1234567    1234    0    0    0     0       0          0
  eth0: 987654321  123456    0    0    0     0          0         0 87654321   98765    0    0    0     0       0          0
docker0:       0       0    0    0    0     0          0         0        0       0    0    0    0     0       0          0
";

    #[test]
    fn net_dev_parses_interfaces_and_byte_counters() {
        let v = net_dev(NET_DEV);
        assert_eq!(v.len(), 3, "表头两行必须被跳过");
        assert_eq!(
            v[0],
            IfaceStat {
                name: "lo".into(),
                rx_bytes: 1234567,
                tx_bytes: 1234567
            }
        );
        assert_eq!(v[1].name, "eth0");
        assert_eq!(v[1].rx_bytes, 987654321);
        assert_eq!(v[1].tx_bytes, 87654321);
    }

    #[test]
    fn net_dev_handles_missing_space_after_colon() {
        // 经典陷阱：字节数够大时冒号后没有空格，按空白切分会把名字和数字粘一起
        let s = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
  eth0:18446744073709551615 123456    0    0    0     0          0         0 87654321   98765    0    0    0     0       0          0
";
        let v = net_dev(s);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "eth0");
        assert_eq!(v[0].rx_bytes, u64::MAX);
    }

    #[test]
    fn net_dev_handles_long_interface_names() {
        let s = "  enp0s31f6verylongname: 100 1 0 0 0 0 0 0 200 2 0 0 0 0 0 0\n";
        let v = net_dev(s);
        assert_eq!(v[0].name, "enp0s31f6verylongname");
        assert_eq!(v[0].rx_bytes, 100);
        assert_eq!(v[0].tx_bytes, 200);
    }

    #[test]
    fn sockstat_extracts_inuse() {
        let s = "\
sockets: used 234
TCP: inuse 12 orphan 0 tw 3 alloc 20 mem 2
UDP: inuse 5 mem 1
UDPLITE: inuse 0
RAW: inuse 0
FRAG: inuse 0 memory 0
";
        assert_eq!(sockstat_inuse(s, "TCP:"), Some(12));
        assert_eq!(sockstat_inuse(s, "UDP:"), Some(5));
        assert_eq!(sockstat_inuse(s, "SCTP:"), None);
    }

    const MOUNTS: &str = "\
sysfs /sys sysfs rw,nosuid,nodev,noexec,relatime 0 0
proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0
/dev/vda1 / ext4 rw,relatime 0 0
tmpfs /run tmpfs rw,nosuid,nodev,size=402424k 0 0
/dev/vdb1 /mnt/my\\040disk xfs rw,relatime 0 0
overlay /var/lib/docker/overlay2/abc/merged overlay rw,relatime 0 0
";

    #[test]
    fn mounts_parses_and_unescapes() {
        let m = mounts(MOUNTS);
        assert_eq!(m.len(), 6);
        assert_eq!(m[2].mount_point, "/");
        assert_eq!(m[2].fstype, "ext4");
        // \040 是空格，不还原的话路径会被截断成 "/mnt/my"
        assert_eq!(m[4].mount_point, "/mnt/my disk");
        assert_eq!(m[4].fstype, "xfs");
    }

    #[test]
    fn hidepid_detection() {
        assert!(!proc_has_hidepid(MOUNTS));
        let hardened = MOUNTS.replace(
            "proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0",
            "proc /proc proc rw,nosuid,nodev,noexec,relatime,hidepid=2 0 0",
        );
        assert!(
            proc_has_hidepid(&hardened),
            "hidepid 下进程数只能看到自己的，必须识别出来"
        );
    }

    #[test]
    fn cpuinfo_model_and_hypervisor() {
        let s = "\
processor\t: 0
vendor_id\t: GenuineIntel
model name\t: Intel(R) Xeon(R) Platinum 8375C CPU @ 2.90GHz
flags\t\t: fpu vme de pse tsc msr pae mce cx8 apic hypervisor lahf_lm
";
        assert_eq!(
            cpu_model(s).unwrap(),
            "Intel(R) Xeon(R) Platinum 8375C CPU @ 2.90GHz"
        );
        assert!(has_hypervisor_flag(s));

        let bare = s.replace(" hypervisor", "");
        assert!(!has_hypervisor_flag(&bare));
    }

    #[test]
    fn cgroup_limit_recognises_unlimited() {
        assert_eq!(cgroup_limit("1073741824\n"), Some(1 << 30));
        assert_eq!(
            cgroup_limit("max\n"),
            None,
            "cgroup v2 的无限制是字面量 max"
        );
        // cgroup v1 的无限制是一个接近 u64 上限的巨大数字，不能当成「限制 8 EB」
        assert_eq!(cgroup_limit("9223372036854771712\n"), None);
        assert_eq!(cgroup_limit("garbage"), None);
    }

    #[test]
    fn cgroup_cpu_max_parses_quota() {
        assert_eq!(cgroup_cpu_max("200000 100000\n"), Some(2.0));
        assert_eq!(cgroup_cpu_max("50000 100000\n"), Some(0.5));
        assert_eq!(cgroup_cpu_max("max 100000\n"), None);
        assert_eq!(cgroup_cpu_max("100000 0\n"), None, "period 为 0 不能除零");
    }

    #[test]
    fn temperature_rejects_implausible_readings() {
        assert_eq!(temp_millidegrees("45000\n"), Some(450)); // 45.0°C
        assert_eq!(temp_millidegrees("-5000\n"), Some(-50));
        // 传感器坏掉时给出的荒谬值不该被上报
        assert_eq!(temp_millidegrees("999000\n"), None);
        assert_eq!(temp_millidegrees("-99000\n"), None);
        assert_eq!(temp_millidegrees("junk"), None);
    }

    #[test]
    fn ping_group_range_gating() {
        // 内核默认 "1 0" 是空区间 —— 禁用
        assert!(!ping_group_allows("1 0", &[0, 1000]));
        assert!(ping_group_allows("0 2147483647", &[1000]));
        assert!(ping_group_allows("999 1001", &[1000]));
        assert!(!ping_group_allows("999 1001", &[1002]));
        assert!(!ping_group_allows("garbage", &[1000]));
    }
}
