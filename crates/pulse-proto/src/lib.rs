//! Pulse agent ↔ server 线协议。
//!
//! agent 与 server 共用**同一份**定义，协议不一致在编译期就会暴露，
//! 而不是等到运行时变成一条看不懂的反序列化错误。
//!
//! 兼容策略：
//! - [`PROTO_VERSION`] 只在**破坏性变更**时递增
//! - 加字段一律带 `#[serde(default)]`，老 agent 连新 server 照常工作
//! - 删字段先保留并忽略，两个大版本后再真删

use serde::{Deserialize, Serialize};

/// 线协议版本。M2 只加了带默认值的字段，属于兼容变更，因此不递增。
pub const PROTO_VERSION: u16 = 1;

/// WS 子协议标识，用于握手时协商。
pub const WS_SUBPROTOCOL: &str = "pulse.v1";

// ---------------------------------------------------------------------------
// 信封
// ---------------------------------------------------------------------------

/// agent → server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum AgentMsg {
    /// 连接后的第一条，携带静态信息与能力声明。
    Hello(Hello),
    /// 周期性指标上报。
    Metrics(Metrics),
    /// 延迟探测结果，**批量**上报（默认 30 秒一批）。
    /// 每次探测都发一条消息的话，6 个目标 × 200 台就是每分钟 1200 条小消息。
    ///
    /// ⚠️ 用具名字段而不是 `PingResults(Vec<..>)`：内部标签枚举
    /// （`#[serde(tag = "t")]`）**不支持直接装序列的新类型变体**，
    /// 序列化时会在运行时失败。这个坑踩过一次，见下方的往返测试。
    PingResults { results: Vec<PingResult> },
}

/// server → agent
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    /// 对 `Hello` 的应答。
    Welcome(Welcome),
    /// 下发运行期配置，秒级生效。
    Config(RuntimeConfig),
    /// 下发延迟探测任务的**全量列表**。增量同步不值得那点带宽 ——
    /// 全量替换语义简单，也不会因为漏了一条删除消息而永远多探一个目标。
    ///
    /// ⚠️ 具名字段的原因同 [`AgentMsg::PingResults`]。
    PingTasks { tasks: Vec<PingTaskSpec> },
    /// 让 agent 升级到某个版本。
    ///
    /// **只有版本号，没有下载地址** —— 这是 R18 的关键防线：
    /// 下载 URL 由 agent 本地配置的 `update_base` 加固定模板拼出，
    /// 面板即使被完全攻陷也无法把 agent 指向自己的服务器。
    /// agent 侧还会验签、校验摘要、并拒绝降级。
    Upgrade { version: String },
}

// ---------------------------------------------------------------------------
// Hello
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub proto_version: u16,
    pub agent_version: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub cpu_cores: u32,
    /// 开机时刻（unix 秒，UTC）。uptime 由 server 用 `now - boot_at` 算，
    /// 这样 agent 重启不会让在线时长归零。
    pub boot_at: i64,

    #[serde(default)]
    pub kernel: Option<String>,
    #[serde(default)]
    pub cpu_model: Option<String>,
    /// kvm / lxc / docker / vmware / none
    #[serde(default)]
    pub virtualization: Option<String>,
    #[serde(default)]
    pub mem_total: u64,
    #[serde(default)]
    pub swap_total: u64,
    #[serde(default)]
    pub disk_total: u64,
    /// 本机全部网卡名，供后台做下拉多选而不是让用户手打网卡名
    #[serde(default)]
    pub interfaces: Vec<String>,
    /// 本机实际能采到什么。后台据此**灰掉**拿不到的开关，
    /// 前端据此**隐藏**对应字段 —— 而不是显示一个 0 去骗人。
    #[serde(default)]
    pub capabilities: Capabilities,
}

/// agent 对自己能力的诚实声明。
///
/// 每一项都是**运行时探测**出来的，不是按平台猜的 ——
/// 同样是 Linux，KVM 上有温度传感器而容器里没有；
/// `/proc` 挂了 `hidepid` 的机器上进程数只能看到自己的。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// 内核允许非特权 ICMP socket（`net.ipv4.ping_group_range` 覆盖本进程 gid）
    #[serde(default)]
    pub icmp_unprivileged: bool,
    #[serde(default)]
    pub gpu_nvml: bool,
    #[serde(default)]
    pub temperature: bool,
    #[serde(default)]
    pub tcp_conn_count: bool,
    /// 进程数可信。`hidepid` 或 `ProtectProc=invisible` 下为 false ——
    /// 此时读到的不是「读不到」而是「一个很小的错数」，必须显式标记
    #[serde(default)]
    pub proc_count: bool,
    /// 系统有负载均值概念（Windows 没有）
    #[serde(default)]
    pub load_average: bool,
    #[serde(default)]
    pub self_update: bool,
    /// 运行在容器里且**未挂 lxcfs** —— 此时 /proc 显示的是宿主机数据，
    /// 已改用 cgroup 限额。后台应提示用户这台机器的规格来自 cgroup。
    #[serde(default)]
    pub cgroup_limited: bool,
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metrics {
    /// agent 本地 unix 秒（UTC）
    pub ts: i64,
    /// CPU 使用率，**万分比** 0..=10000
    pub cpu_pct: u16,

    /// 1/5/15 分钟负载 ×100。`capabilities.load_average == false` 时无意义
    #[serde(default)]
    pub load: [u16; 3],

    /// 内存**原始值**。含不含 buff/cache 的口径由 server 端决定，
    /// 所以后台改开关时历史数据也一并按新口径重新呈现，无需重装 agent。
    #[serde(default)]
    pub mem: Mem,

    #[serde(default)]
    pub disk: DiskUsage,
    #[serde(default)]
    pub net: NetStat,

    #[serde(default)]
    pub tcp_conn: Option<u32>,
    #[serde(default)]
    pub udp_conn: Option<u32>,
    #[serde(default)]
    pub proc_count: Option<u32>,
    #[serde(default)]
    pub gpu: Option<GpuStat>,
    /// 最高的那个温度传感器读数，×10 摄氏度
    #[serde(default)]
    pub cpu_temp: Option<i32>,
    #[serde(default)]
    pub uptime_s: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mem {
    pub total: u64,
    pub free: u64,
    /// Linux 的 `MemAvailable`。没有这个概念的平台填等价估算值。
    pub available: u64,
    pub buffers: u64,
    pub cached: u64,
    pub swap_total: u64,
    pub swap_free: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskUsage {
    /// 参与统计的挂载点合计
    pub total: u64,
    pub used: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetStat {
    /// 参与统计的网卡（已按白/黑名单过滤）的**累计**字节数之和。
    ///
    /// 上报累计值而不是增量：server 端算 delta 才能正确处理 agent 掉线、
    /// 机器重启、agent 重装三种情况。
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// 本采样周期内的瞬时速率 B/s，由 agent 本地算 —— server 端算会受上报抖动影响
    pub rx_speed: u64,
    pub tx_speed: u64,
    /// 参与统计的网卡名。集合变化时 server 会重置流量基线，
    /// 否则改过滤规则会造成一次巨大的假跳变。
    #[serde(default)]
    pub ifaces: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuStat {
    /// 万分比
    pub util: u16,
    pub mem_used: u64,
    pub mem_total: u64,
    /// ×10 摄氏度
    pub temp: i32,
}

// ---------------------------------------------------------------------------
// server → agent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Welcome {
    /// server 时间，供 agent 校正时钟偏移
    pub server_time: i64,
    /// 采集间隔（秒）
    pub interval_s: u8,
}

/// 运行期配置：后台改完 WS 下发，**≤2 秒生效，不用碰机器**。
///
/// 安全约束（R18）：这里的每个字段都是数值、布尔或 glob 字符串，
/// **没有任何一个能变成可执行内容**。agent 侧还会再做一次范围与长度校验。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    /// 1..=60，agent 侧 clamp
    pub interval_s: u8,
    /// 网卡白名单（glob）。空 = 不限制
    #[serde(default)]
    pub net_include: Vec<String>,
    /// 网卡黑名单（glob）
    #[serde(default)]
    pub net_exclude: Vec<String>,
    /// 磁盘挂载点白名单。空 = 自动（只统计真实块设备文件系统）
    #[serde(default)]
    pub disk_include: Vec<String>,
    #[serde(default)]
    pub disk_exclude: Vec<String>,
    #[serde(default)]
    pub gpu_enabled: bool,
    #[serde(default = "yes")]
    pub report_temps: bool,
    #[serde(default = "yes")]
    pub report_conn_count: bool,
}

fn yes() -> bool {
    true
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            interval_s: 2,
            net_include: Vec::new(),
            net_exclude: default_net_exclude()
                .iter()
                .map(|s| s.to_string())
                .collect(),
            disk_include: Vec::new(),
            disk_exclude: Vec::new(),
            gpu_enabled: false,
            report_temps: true,
            report_conn_count: true,
        }
    }
}

/// 用户未配置时的默认网卡黑名单：回环、容器、虚拟网桥、隧道、VPN。
///
/// 不排除的话，Docker 主机的流量会被 `veth*` 重复计算好几遍。
pub const fn default_net_exclude() -> &'static [&'static str] {
    &[
        "lo", "lo0", "veth*", "docker*", "br-*", "virbr*", "tun*", "tap*", "kube*", "cni*",
        "flannel*", "zt*", "wg*", "utun*", "awdl*", "llw*", "bridge*", "vmnet*", "gif*", "stf*",
    ]
}

/// agent 侧对下发配置的强制校验。
///
/// server 被攻破也不能让 agent 做出格的事 —— 数值 clamp 到合法区间，
/// glob 模式限长限量，超出的直接丢弃。
impl RuntimeConfig {
    pub const MAX_PATTERNS: usize = 32;
    pub const MAX_PATTERN_LEN: usize = 64;

    pub fn sanitize(mut self) -> Self {
        self.interval_s = self.interval_s.clamp(1, 60);
        for v in [
            &mut self.net_include,
            &mut self.net_exclude,
            &mut self.disk_include,
            &mut self.disk_exclude,
        ] {
            v.retain(|p| !p.is_empty() && p.len() <= Self::MAX_PATTERN_LEN);
            v.truncate(Self::MAX_PATTERNS);
        }
        self
    }
}

// ---------------------------------------------------------------------------
// 延迟监控
// ---------------------------------------------------------------------------

/// 探测方式。
///
/// **默认是 TCP 而不是 ICMP**：ICMP raw socket 需要 `CAP_NET_RAW`，
/// 与 R18「零 capability」直接冲突。Linux 上若内核允许非特权 ICMP
/// （`net.ipv4.ping_group_range` 覆盖运行用户的 gid）才可以选 ICMP，
/// 否则 agent 会自动回落到 TCP:443 并在结果上打标记。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PingKind {
    Tcp {
        port: u16,
    },
    Icmp,
    Http {
        /// 期望的状态码。`None` = 只要有响应就算通
        expect_status: Option<u16>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingTaskSpec {
    pub id: u32,
    pub name: String,
    #[serde(flatten)]
    pub kind: PingKind,
    /// tcp/icmp 是主机名或 IP；http 是完整 URL
    pub host: String,
    pub interval_s: u16,
    /// 每轮发几个包，用来算丢包率
    pub packets: u8,
    pub timeout_ms: u16,
}

impl PingTaskSpec {
    pub const MAX_PACKETS: u8 = 10;
    pub const MIN_INTERVAL_S: u16 = 10;

    /// agent 侧的强制校验。
    ///
    /// 下发的任务会变成真实的出网连接，所以数量与频率必须夹紧 ——
    /// 否则一条配置就能让 200 台机器同时对某个地址发起洪水。
    pub fn sanitize(mut self) -> Self {
        self.interval_s = self.interval_s.clamp(Self::MIN_INTERVAL_S, 3600);
        self.packets = self.packets.clamp(1, Self::MAX_PACKETS);
        self.timeout_ms = self.timeout_ms.clamp(100, 10_000);
        self.host.truncate(255);
        self.name.truncate(64);
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingResult {
    pub task_id: u32,
    /// 分钟对齐的 unix 秒
    pub ts: i64,
    pub sent: u16,
    pub recv: u16,
    /// 微秒。全部丢包时为 None
    pub rtt_min_us: Option<u32>,
    pub rtt_avg_us: Option<u32>,
    pub rtt_max_us: Option<u32>,
    /// 该任务本应走 ICMP，但本机不支持非特权 ICMP，已回落 TCP。
    /// 后台据此提示用户，而不是让他对着一个语义不同的数字发呆。
    #[serde(default)]
    pub fallback: bool,
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

/// 把 0.0..=100.0 的百分比转成万分比整数，并 clamp 到合法范围。
///
/// 输入可能来自各平台的采集 API，NaN / 越界都得能扛住而不是 panic。
pub fn pct_to_basis_points(pct: f32) -> u16 {
    // NaN 不带任何信息，归 0；±∞ 是带方向的，交给 clamp 落到对应边界。
    if pct.is_nan() {
        return 0;
    }
    (pct * 100.0).clamp(0.0, 10_000.0).round() as u16
}

/// 万分比 → 百分比，用于展示。
pub fn basis_points_to_pct(bp: u16) -> f32 {
    f32::from(bp) / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pct_roundtrip() {
        assert_eq!(pct_to_basis_points(0.0), 0);
        assert_eq!(pct_to_basis_points(12.34), 1234);
        assert_eq!(pct_to_basis_points(100.0), 10_000);
        assert!((basis_points_to_pct(1234) - 12.34).abs() < 0.001);
    }

    #[test]
    fn pct_clamps_out_of_range() {
        // 越界与非有限值必须被吃掉而不是 panic —— 采集 API 返回什么都不奇怪
        assert_eq!(pct_to_basis_points(-1.0), 0);
        assert_eq!(pct_to_basis_points(101.0), 10_000);
        assert_eq!(pct_to_basis_points(f32::NAN), 0);
        assert_eq!(pct_to_basis_points(f32::INFINITY), 10_000);
        assert_eq!(pct_to_basis_points(f32::NEG_INFINITY), 0);
    }

    #[test]
    fn envelope_roundtrip() {
        let m = AgentMsg::Metrics(Metrics {
            ts: 1_756_800_000,
            cpu_pct: 1234,
            ..Default::default()
        });
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains(r#""t":"metrics""#), "tag 必须是 snake_case: {s}");
        let back: AgentMsg = serde_json::from_str(&s).unwrap();
        match back {
            AgentMsg::Metrics(x) => assert_eq!(x.cpu_pct, 1234),
            _ => panic!("变体错了"),
        }
    }

    #[test]
    fn unknown_variant_is_rejected_not_panicked() {
        let r: Result<AgentMsg, _> = serde_json::from_str(r#"{"t":"nope"}"#);
        assert!(r.is_err());
    }

    #[test]
    fn old_agent_message_still_parses() {
        // 兼容性回归：M0/M1 的 agent 只发 ts + cpu_pct。
        // 新 server 必须照常接收，缺的字段走 serde(default)。
        let old = r#"{"t":"metrics","ts":1756800000,"cpu_pct":1234}"#;
        let m: AgentMsg = serde_json::from_str(old).expect("老格式必须仍能解析");
        match m {
            AgentMsg::Metrics(x) => {
                assert_eq!(x.cpu_pct, 1234);
                assert_eq!(x.mem, Mem::default());
                assert_eq!(x.proc_count, None);
            }
            _ => panic!("变体错了"),
        }

        let old_hello = r#"{"t":"hello","proto_version":1,"agent_version":"0.0.1",
            "hostname":"h","os":"Linux","arch":"x86_64","cpu_cores":2,"boot_at":0}"#;
        let h: AgentMsg = serde_json::from_str(old_hello).expect("老 Hello 必须仍能解析");
        match h {
            AgentMsg::Hello(x) => {
                assert_eq!(x.capabilities, Capabilities::default());
                assert!(x.interfaces.is_empty());
            }
            _ => panic!("变体错了"),
        }
    }

    #[test]
    fn runtime_config_sanitize_clamps_hostile_input() {
        // server 被攻破也不能让 agent 做出格的事
        let cfg = RuntimeConfig {
            interval_s: 0,
            net_exclude: (0..1000).map(|i| format!("pattern{i}")).collect(),
            net_include: vec!["x".repeat(500), String::new(), "eth0".into()],
            ..Default::default()
        }
        .sanitize();

        assert_eq!(cfg.interval_s, 1, "0 秒间隔必须被 clamp");
        assert_eq!(cfg.net_exclude.len(), RuntimeConfig::MAX_PATTERNS);
        assert_eq!(cfg.net_include, vec!["eth0"], "超长与空模式必须被丢弃");

        let cfg = RuntimeConfig {
            interval_s: 255,
            ..Default::default()
        }
        .sanitize();
        assert_eq!(cfg.interval_s, 60);
    }

    #[test]
    fn every_message_variant_survives_a_roundtrip() {
        // 这条测试是为一个真实的坑加的：`#[serde(tag = "t")]` 的内部标签枚举
        // **不支持直接装序列的新类型变体**（`PingResults(Vec<..>)`），
        // 序列化会在**运行时**失败。而当时调用处用了
        // `let Ok(txt) = to_string(..) else { continue }`，把错误静默吞掉了 ——
        // 表现成「任务下发了但 agent 收不到」，查了很久。
        //
        // 所以：每个变体都必须在这里走一遍完整往返。
        let hello = Hello {
            proto_version: PROTO_VERSION,
            agent_version: "0.0.1".into(),
            hostname: "h".into(),
            os: "Linux".into(),
            arch: "x86_64".into(),
            cpu_cores: 2,
            boot_at: 0,
            kernel: None,
            cpu_model: None,
            virtualization: None,
            mem_total: 0,
            swap_total: 0,
            disk_total: 0,
            interfaces: vec!["eth0".into()],
            capabilities: Capabilities::default(),
        };
        let spec = PingTaskSpec {
            id: 1,
            name: "t".into(),
            kind: PingKind::Tcp { port: 443 },
            host: "1.1.1.1".into(),
            interval_s: 60,
            packets: 3,
            timeout_ms: 3000,
        };
        let result = PingResult {
            task_id: 1,
            ts: 60,
            sent: 3,
            recv: 3,
            ..Default::default()
        };

        for m in [
            AgentMsg::Hello(hello),
            AgentMsg::Metrics(Metrics::default()),
            AgentMsg::PingResults {
                results: vec![result],
            },
        ] {
            let s = serde_json::to_string(&m).unwrap_or_else(|e| panic!("序列化 {m:?} 失败: {e}"));
            serde_json::from_str::<AgentMsg>(&s)
                .unwrap_or_else(|e| panic!("反序列化 {s} 失败: {e}"));
        }

        for m in [
            ServerMsg::Welcome(Welcome {
                server_time: 0,
                interval_s: 2,
            }),
            ServerMsg::Config(RuntimeConfig::default()),
            ServerMsg::PingTasks { tasks: vec![spec] },
            ServerMsg::Upgrade {
                version: "0.4.0".into(),
            },
        ] {
            let s = serde_json::to_string(&m).unwrap_or_else(|e| panic!("序列化 {m:?} 失败: {e}"));
            serde_json::from_str::<ServerMsg>(&s)
                .unwrap_or_else(|e| panic!("反序列化 {s} 失败: {e}"));
        }
    }

    #[test]
    fn ping_task_sanitize_clamps_hostile_values() {
        // 下发的任务会变成真实的出网连接：一条配置就能让 200 台机器
        // 同时对某个地址发洪水，所以频率与包数必须夹紧
        let t = PingTaskSpec {
            id: 1,
            name: "x".repeat(200),
            kind: PingKind::Tcp { port: 443 },
            host: "h".repeat(500),
            interval_s: 0,
            packets: 255,
            timeout_ms: 0,
        }
        .sanitize();
        assert_eq!(
            t.interval_s,
            PingTaskSpec::MIN_INTERVAL_S,
            "间隔不能低于下限"
        );
        assert_eq!(t.packets, PingTaskSpec::MAX_PACKETS);
        assert_eq!(t.timeout_ms, 100);
        assert_eq!(t.host.len(), 255);
        assert_eq!(t.name.len(), 64);

        let t = PingTaskSpec {
            id: 1,
            name: "ok".into(),
            kind: PingKind::Icmp,
            host: "1.1.1.1".into(),
            interval_s: u16::MAX,
            packets: 3,
            timeout_ms: u16::MAX,
        }
        .sanitize();
        assert_eq!(t.interval_s, 3600);
        assert_eq!(t.timeout_ms, 10_000);
    }

    #[test]
    fn ping_kind_serialises_flat() {
        // kind 与 spec 扁平化在同一层，前端拿到的 JSON 更好写
        let t = PingTaskSpec {
            id: 7,
            name: "CF".into(),
            kind: PingKind::Tcp { port: 443 },
            host: "1.1.1.1".into(),
            interval_s: 60,
            packets: 3,
            timeout_ms: 3000,
        };
        let s = serde_json::to_string(&t).unwrap();
        assert!(s.contains(r#""kind":"tcp""#), "{s}");
        assert!(s.contains(r#""port":443"#), "{s}");
        let back: PingTaskSpec = serde_json::from_str(&s).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn ping_result_defaults_fallback_false() {
        // 老 agent 不发这个字段
        let r: PingResult = serde_json::from_str(
            r#"{"task_id":1,"ts":60,"sent":3,"recv":3,"rtt_min_us":1,"rtt_avg_us":2,"rtt_max_us":3}"#
        ).unwrap();
        assert!(!r.fallback);
    }

    #[test]
    fn default_net_exclude_covers_common_virtual_interfaces() {
        let d = default_net_exclude();
        for want in ["lo", "veth*", "docker*", "br-*", "utun*"] {
            assert!(d.contains(&want), "默认黑名单缺少 {want}");
        }
    }
}
