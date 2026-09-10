//! 存储层抽象。
//!
//! 所有具体后端（SQLite / PostgreSQL）都实现 [`Storage`]，上层只认这个 trait。
//! 设计约束来自 ：
//!
//! - **写只有批量接口**，没有单行写。单行独立事务是 SQLite 慢的根源。
//! - **读必须带时间范围与粒度**，由 [`plan_query`] 决定走哪一层，
//!   保证任何跨度返回的点数都是恒定量级。

pub mod sqlite;

use async_trait::async_trait;

pub type ServerId = i64;
pub type Result<T> = std::result::Result<T, sqlx::Error>;

/// 单次查询返回的最大点数。
///
/// 前端图表画不下更多点，后端也没必要传更多。所有跨度都被压到这个量级之内 ——
/// 这是"30 天曲线和 6 小时曲线一样快"的根本原因。
pub const MAX_POINTS: i64 = 750;

// ---------------------------------------------------------------------------
// 行类型
// ---------------------------------------------------------------------------

/// 分钟层 / 小时层共用的一行。
///
/// 全部用 `i64`：SQLite 的 INTEGER 本来就是 i64，避免来回转换。
/// 百分比是万分比、负载 ×100、温度 ×10 —— 定点整数，见 migrations 里的注释。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetricRow {
    pub server_id: ServerId,
    pub ts: i64,
    pub cpu_pct: Option<i64>,
    pub cpu_pct_max: Option<i64>,
    pub load1: Option<i64>,
    pub mem_total: Option<i64>,
    pub mem_free: Option<i64>,
    pub mem_available: Option<i64>,
    pub swap_used: Option<i64>,
    pub disk_used: Option<i64>,
    pub disk_total: Option<i64>,
    pub net_in_speed: Option<i64>,
    pub net_out_speed: Option<i64>,
    pub net_in_peak: Option<i64>,
    pub net_out_peak: Option<i64>,
    pub net_in_total: Option<i64>,
    pub net_out_total: Option<i64>,
    pub tcp_conn: Option<i64>,
    pub udp_conn: Option<i64>,
    pub proc_count: Option<i64>,
    pub gpu_util: Option<i64>,
    pub gpu_mem_used: Option<i64>,
    pub gpu_temp: Option<i64>,
    pub cpu_temp: Option<i64>,
}

/// 三个延迟层共用的一行。
///
/// 注意存的是 `sent`/`recv` 计数而不是丢包百分比 —— 上卷时求和即可，
/// 而不同样本数的百分比直接平均是错的。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PingRow {
    pub server_id: ServerId,
    pub task_id: i64,
    pub ts: i64,
    /// 微秒
    pub rtt_avg: Option<i64>,
    pub rtt_min: Option<i64>,
    pub rtt_max: Option<i64>,
    pub sent: i64,
    pub recv: i64,
}

// ---------------------------------------------------------------------------
// 列定义
//
// 插入的列顺序、绑定顺序、上卷的聚合方式、内存层的聚合方式，全部由这一份常量派生。
// 在四个地方各写一遍列名，是"值悄悄进错列"这类静默 bug 的经典来源 ——
// SQL 不会报错，只会把数字塞进相邻的列。
// ---------------------------------------------------------------------------

/// 一列在上卷 / 聚合时的合并方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agg {
    /// 平均值：使用率、占用量这类"瞬时状态"
    Avg,
    /// 最大值：峰值、累计计数这类"取窗口内最后/最高"
    Max,
}

impl Agg {
    /// 生成该列在 SQL 上卷时的聚合表达式。
    pub fn sql(self, col: &str) -> String {
        match self {
            // CAST 回 INTEGER：AVG 返回浮点，直接存会让整列变成 REAL，
            // 行宽和后续聚合精度都会跟着变
            Agg::Avg => format!("CAST(AVG({col}) AS INTEGER)"),
            Agg::Max => format!("MAX({col})"),
        }
    }
}

pub const METRIC_COLS: &[(&str, Agg)] = &[
    ("cpu_pct", Agg::Avg),
    ("cpu_pct_max", Agg::Max),
    ("load1", Agg::Avg),
    ("mem_total", Agg::Avg),
    ("mem_free", Agg::Avg),
    ("mem_available", Agg::Avg),
    ("swap_used", Agg::Avg),
    ("disk_used", Agg::Avg),
    ("disk_total", Agg::Avg),
    ("net_in_speed", Agg::Avg),
    ("net_out_speed", Agg::Avg),
    ("net_in_peak", Agg::Max),
    ("net_out_peak", Agg::Max),
    ("net_in_total", Agg::Max),
    ("net_out_total", Agg::Max),
    ("tcp_conn", Agg::Avg),
    ("udp_conn", Agg::Avg),
    ("proc_count", Agg::Avg),
    ("gpu_util", Agg::Avg),
    ("gpu_mem_used", Agg::Avg),
    ("gpu_temp", Agg::Avg),
    ("cpu_temp", Agg::Avg),
];

/// 可空指标列的数量。与 [`METRIC_COLS`] 的长度由单元测试锁死。
pub const METRIC_COL_COUNT: usize = 22;

pub fn metric_col_list() -> String {
    METRIC_COLS
        .iter()
        .map(|(c, _)| *c)
        .collect::<Vec<_>>()
        .join(", ")
}

impl MetricRow {
    /// 顺序必须与 [`METRIC_COLS`] 严格一致，由单元测试守住。
    pub fn values(&self) -> [Option<i64>; METRIC_COL_COUNT] {
        [
            self.cpu_pct,
            self.cpu_pct_max,
            self.load1,
            self.mem_total,
            self.mem_free,
            self.mem_available,
            self.swap_used,
            self.disk_used,
            self.disk_total,
            self.net_in_speed,
            self.net_out_speed,
            self.net_in_peak,
            self.net_out_peak,
            self.net_in_total,
            self.net_out_total,
            self.tcp_conn,
            self.udp_conn,
            self.proc_count,
            self.gpu_util,
            self.gpu_mem_used,
            self.gpu_temp,
            self.cpu_temp,
        ]
    }

    /// [`values`](Self::values) 的逆运算。同样依赖顺序一致。
    pub fn from_values(server_id: ServerId, ts: i64, v: [Option<i64>; METRIC_COL_COUNT]) -> Self {
        Self {
            server_id,
            ts,
            cpu_pct: v[0],
            cpu_pct_max: v[1],
            load1: v[2],
            mem_total: v[3],
            mem_free: v[4],
            mem_available: v[5],
            swap_used: v[6],
            disk_used: v[7],
            disk_total: v[8],
            net_in_speed: v[9],
            net_out_speed: v[10],
            net_in_peak: v[11],
            net_out_peak: v[12],
            net_in_total: v[13],
            net_out_total: v[14],
            tcp_conn: v[15],
            udp_conn: v[16],
            proc_count: v[17],
            gpu_util: v[18],
            gpu_mem_used: v[19],
            gpu_temp: v[20],
            cpu_temp: v[21],
        }
    }
}

/// 把一个时间窗内的若干采样聚合成一行。
///
/// 这是内存实时层 → 分钟层的降采样，与 SQL 里分钟层 → 小时层的上卷
/// 使用**同一份** [`METRIC_COLS`] 聚合定义，保证两级降采样语义一致。
pub fn aggregate_window(samples: &[MetricRow], server_id: ServerId, ts: i64) -> MetricRow {
    let mut out = [None; METRIC_COL_COUNT];
    for (i, (_, agg)) in METRIC_COLS.iter().enumerate() {
        let vals = samples.iter().filter_map(|s| s.values()[i]);
        out[i] = match agg {
            Agg::Max => vals.max(),
            Agg::Avg => {
                let (sum, n) = vals.fold((0i128, 0i64), |(s, n), v| (s + i128::from(v), n + 1));
                // 全是 None 时返回 None，而不是 0 —— 0 是个有意义的值，
                // 用它表示"没数据"会让前端画出一条假的零线
                (n > 0).then(|| (sum / i128::from(n)) as i64)
            }
        };
    }
    MetricRow::from_values(server_id, ts, out)
}

// ---------------------------------------------------------------------------
// 查询计划
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    Minute,
    Hour,
}

impl Granularity {
    /// 该层原始行的时间步长（秒）。
    pub const fn base_step(self) -> i64 {
        match self {
            Granularity::Minute => 60,
            Granularity::Hour => 3600,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryPlan {
    pub granularity: Granularity,
    /// 实际返回的时间步长（秒），是 `base_step` 的整数倍。
    pub step: i64,
}

/// 向上取整除法。`i64::div_ceil` 目前仍是 unstable，自己写一个。
/// 只用于正数，调用方保证 `b > 0`。
const fn ceil_div(a: i64, b: i64) -> i64 {
    (a + b - 1) / b
}

/// 按查询跨度选层与抽稀倍数。
///
/// 前端只传时间范围，不需要知道分层的存在 —— 这是存储分层对上层透明的关键。
pub fn plan_query(range_secs: i64) -> QueryPlan {
    const SEVEN_DAYS: i64 = 7 * 86_400;
    let granularity = if range_secs <= SEVEN_DAYS {
        Granularity::Minute
    } else {
        Granularity::Hour
    };
    let base = granularity.base_step();
    let raw_points = (range_secs / base).max(1);
    // 向上取整的抽稀倍数，保证结果点数 ≤ MAX_POINTS
    let factor = ceil_div(raw_points, MAX_POINTS).max(1);
    QueryPlan {
        granularity,
        step: base * factor,
    }
}

// ---------------------------------------------------------------------------
// 查询结果（列式）
// ---------------------------------------------------------------------------

/// 列式而不是对象数组：720 个点的响应体从约 180 KB 降到约 25 KB，
/// 且 uPlot 直接吃这个格式无需转换。
#[derive(Debug, Default, serde::Serialize)]
pub struct MetricSeries {
    pub granularity: Option<Granularity>,
    pub step: i64,
    pub ts: Vec<i64>,
    pub cpu_pct: Vec<Option<i64>>,
    pub mem_total: Vec<Option<i64>>,
    pub mem_available: Vec<Option<i64>>,
    pub disk_used: Vec<Option<i64>>,
    pub disk_total: Vec<Option<i64>>,
    pub net_in_speed: Vec<Option<i64>>,
    pub net_out_speed: Vec<Option<i64>>,
    /// 1 分钟负载 ×100。平台没有负载概念时整列为 None
    pub load1: Vec<Option<i64>>,
    pub swap_used: Vec<Option<i64>>,
    pub tcp_conn: Vec<Option<i64>>,
    pub udp_conn: Vec<Option<i64>>,
    pub proc_count: Vec<Option<i64>>,
}

impl MetricSeries {
    pub fn len(&self) -> usize {
        self.ts.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ts.is_empty()
    }
}

#[derive(Debug, Default, serde::Serialize)]
pub struct PingSeries {
    pub granularity: Option<Granularity>,
    pub step: i64,
    pub ts: Vec<i64>,
    pub rtt_avg: Vec<Option<i64>>,
    pub rtt_min: Vec<Option<i64>>,
    pub rtt_max: Vec<Option<i64>>,
    /// 丢包率，**真百分比**（0..=100），由 sent/recv 现算。
    ///
    /// 曾经是「百分比 ×100」的整数（10000 = 100%），而实时那条路
    /// （`state::LatencyView::loss_pct`）一直是真百分比 —— 同名不同单位，
    /// 结果详情页的丢包图 y 轴画到了 10000。两边统一成真百分比。
    pub loss_pct: Vec<Option<f64>>,
}

impl PingSeries {
    pub fn len(&self) -> usize {
        self.ts.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ts.is_empty()
    }
}

// ---------------------------------------------------------------------------
// 分层
// ---------------------------------------------------------------------------

/// 指标的两层。上层代码通常只写 `Minute`，`Hour` 由上卷任务产生 ——
/// 但压测需要直接按稳态体量写各层，所以接口对两层都开放。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricLayer {
    Minute,
    Hour,
}

impl MetricLayer {
    pub const fn table(self) -> &'static str {
        match self {
            MetricLayer::Minute => "metrics_minute",
            MetricLayer::Hour => "metrics_hour",
        }
    }
}

/// 延迟的三层。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PingLayer {
    Raw,
    M5,
    Hour,
}

impl PingLayer {
    pub const fn table(self) -> &'static str {
        match self {
            PingLayer::Raw => "ping_raw",
            PingLayer::M5 => "ping_5m",
            PingLayer::Hour => "ping_hour",
        }
    }
}

// ---------------------------------------------------------------------------
// 保留策略与运维报告
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct RetentionPolicy {
    pub minute_days: i64,
    pub hour_days: i64,
    pub ping_raw_hours: i64,
    pub ping_5m_days: i64,
    pub ping_hour_days: i64,
}

impl Default for RetentionPolicy {
    /// 默认值来自 的容量测算。
    /// `minute_days` 是最有效的调节旋钮：降到 3 天可省 60% 而不损失可见精度。
    fn default() -> Self {
        Self {
            minute_days: 7,
            hour_days: 395, // 13 个月
            ping_raw_hours: 24,
            ping_5m_days: 7,
            ping_hour_days: 30,
        }
    }
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct PruneReport {
    pub metrics_minute: u64,
    pub metrics_hour: u64,
    pub ping_raw: u64,
    pub ping_5m: u64,
    pub ping_hour: u64,
    pub chunks: u64,
    pub max_chunk_ms: u128,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct RollupReport {
    pub metrics_hour: u64,
    pub ping_5m: u64,
    pub ping_hour: u64,
    /// 全部层都已追平到当前时刻。为 false 说明还有积压，调用方应立即再跑一轮。
    pub caught_up: bool,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct StorageStats {
    pub driver: &'static str,
    pub size_bytes: i64,
    pub page_size: i64,
    pub page_count: i64,
    pub freelist_count: i64,
    pub rows: std::collections::BTreeMap<String, i64>,
}

// ---------------------------------------------------------------------------
// 管理相关的记录类型
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AdminUser {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
}

/// 一台服务器的完整记录。
///
/// 注意**没有 token 明文字段** —— 数据库里只有 `token_hash`，
/// 明文只在创建/重置时返回一次。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ServerRecord {
    pub id: ServerId,
    pub uuid: String,
    pub name: String,
    pub group_id: Option<i64>,
    pub sort_order: i64,
    /// true = 不在公开页展示
    pub hidden: bool,
    pub note: Option<String>,
    pub buy_url: Option<String>,
    pub review_url: Option<String>,

    pub country_code: Option<String>,
    pub region: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// true = 人工填过，GeoIP 不再改写
    pub location_manual: bool,
    pub asn: Option<String>,
    pub isp: Option<String>,
    /// agent 连入的源 IP。**公开接口默认不暴露**
    pub last_ip: Option<String>,

    pub os: Option<String>,
    pub kernel: Option<String>,
    pub arch: Option<String>,
    pub virtualization: Option<String>,
    pub cpu_model: Option<String>,
    pub cpu_cores: Option<i64>,
    pub mem_total: Option<i64>,
    pub swap_total: Option<i64>,
    pub disk_total: Option<i64>,
    pub agent_version: Option<String>,
    pub boot_at: Option<i64>,
    /// agent 声明的能力，原样透传的 JSON
    pub capabilities: Option<String>,

    pub first_seen_at: i64,
    pub last_seen_at: Option<i64>,
}

/// [`Storage::all_billing`] 的一行。
#[derive(Debug, Clone)]
pub struct BillingEntry {
    pub id: ServerId,
    pub uuid: String,
    pub name: String,
    pub country_code: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub group_id: Option<i64>,
    /// 购买同款 / 测评链接（R16）。**没装探针的机器也要能显示这两个**，
    /// 所以要跟着价值缓存走，不能只放在 agent 连上后才有的内存态里。
    pub buy_url: Option<String>,
    pub review_url: Option<String>,
    /// 不在公开页展示
    pub hidden: bool,
    // agent 上报过的静态信息。从没上线的机器这些是 None，
    // 前端显示「未上线」而不是空白
    pub os: Option<String>,
    pub arch: Option<String>,
    pub cpu_cores: Option<i64>,
    pub mem_total: Option<i64>,
    pub disk_total: Option<i64>,
    pub agent_version: Option<String>,
    pub last_seen_at: Option<i64>,
    pub billing: Option<crate::domain::billing::Billing>,
}

/// 服务器的可编辑字段。`None` 表示不改这一项 ——
/// 和「改成空值」是两回事，所以用嵌套 Option 表达「清空」。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ServerPatch {
    pub name: Option<String>,
    pub group_id: Option<Option<i64>>,
    pub hidden: Option<bool>,
    pub note: Option<Option<String>>,
    pub buy_url: Option<Option<String>>,
    pub review_url: Option<Option<String>>,
    pub country_code: Option<Option<String>>,
    pub region: Option<Option<String>>,
    pub latitude: Option<Option<f64>>,
    pub longitude: Option<Option<f64>>,
    pub location_manual: Option<bool>,
}

/// agent `Hello` 带来的静态信息，连接时写回数据库。
#[derive(Debug, Clone, Default)]
pub struct ServerFacts {
    pub name: Option<String>,
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub arch: Option<String>,
    pub virtualization: Option<String>,
    pub cpu_model: Option<String>,
    pub cpu_cores: Option<i64>,
    pub mem_total: Option<i64>,
    pub swap_total: Option<i64>,
    pub disk_total: Option<i64>,
    pub agent_version: Option<String>,
    pub boot_at: Option<i64>,
    pub capabilities: Option<String>,
    pub last_ip: Option<String>,
}

// ---------------------------------------------------------------------------
// 延迟监控任务
// ---------------------------------------------------------------------------

/// 任务的作用范围。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "scope_kind", content = "scope_ids", rename_all = "snake_case")]
pub enum PingScope {
    /// 全部机器
    #[default]
    All,
    /// 指定分组
    Group(Vec<i64>),
    /// 指定机器
    Servers(Vec<ServerId>),
}

impl PingScope {
    /// 这台机器是否在作用范围内。
    pub fn covers(&self, server_id: ServerId, group_id: Option<i64>) -> bool {
        match self {
            PingScope::All => true,
            PingScope::Group(gs) => group_id.is_some_and(|g| gs.contains(&g)),
            PingScope::Servers(ss) => ss.contains(&server_id),
        }
    }

    /// 拆成数据库里的两列。
    pub fn to_columns(&self) -> (&'static str, String) {
        match self {
            PingScope::All => ("all", "[]".into()),
            PingScope::Group(g) => ("group", serde_json::to_string(g).unwrap_or_default()),
            PingScope::Servers(s) => ("servers", serde_json::to_string(s).unwrap_or_default()),
        }
    }

    pub fn from_columns(kind: &str, ids: &str) -> Self {
        let parse = || serde_json::from_str::<Vec<i64>>(ids).unwrap_or_default();
        match kind {
            "group" => PingScope::Group(parse()),
            "servers" => PingScope::Servers(parse()),
            // 未知的 scope_kind 一律当成「全部」而不是「无」——
            // 数据被改坏时宁可多探也不要静默地什么都不探
            _ => PingScope::All,
        }
    }
}

/// 后台提交的任务定义。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PingTaskInput {
    pub name: String,
    /// tcp | icmp | http
    pub kind: String,
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub expect_status: Option<u16>,
    #[serde(default = "default_ping_interval")]
    pub interval_s: u16,
    #[serde(default = "default_packets")]
    pub packets: u8,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u16,
    /// `all` | `group` | `servers`。缺省为 `all`。
    ///
    /// 这里不用 `#[serde(flatten)]` + 带标签枚举：那样在 `scope_kind` 缺失时
    /// serde **不会**走 `default`，而是直接报 422。对一个「不填就是全部」的
    /// 字段来说，那是个很差的接口。
    #[serde(default)]
    pub scope_kind: Option<String>,
    #[serde(default)]
    pub scope_ids: Option<Vec<i64>>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

impl PingTaskInput {
    pub fn scope(&self) -> PingScope {
        match self.scope_kind.as_deref() {
            Some(k) => PingScope::from_columns(
                k,
                &serde_json::to_string(self.scope_ids.as_deref().unwrap_or(&[]))
                    .unwrap_or_else(|_| "[]".into()),
            ),
            None => PingScope::All,
        }
    }
}

fn default_ping_interval() -> u16 {
    60
}
fn default_packets() -> u8 {
    3
}
fn default_timeout() -> u16 {
    3000
}
fn yes() -> bool {
    true
}

/// 数据库里的一条完整任务记录。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PingTaskRow {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub host: String,
    pub port: Option<i64>,
    pub expect_status: Option<i64>,
    pub interval_s: i64,
    pub packets: i64,
    pub timeout_ms: i64,
    #[serde(flatten)]
    pub scope: PingScope,
    pub enabled: bool,
    pub created_at: i64,
}

impl PingTaskRow {
    /// 转成下发给 agent 的规格。
    ///
    /// `kind` 是数据库里的字符串，非法值一律回落 TCP:443 ——
    /// 探针宁可探一个可能没意义的目标，也不该因为一条坏数据整个不工作。
    pub fn to_spec(&self) -> pulse_proto::PingTaskSpec {
        use pulse_proto::PingKind;
        let kind = match self.kind.as_str() {
            "icmp" => PingKind::Icmp,
            "http" => PingKind::Http {
                expect_status: self.expect_status.map(|v| v as u16),
            },
            _ => PingKind::Tcp {
                port: self.port.unwrap_or(443).clamp(1, 65535) as u16,
            },
        };
        pulse_proto::PingTaskSpec {
            id: self.id as u32,
            name: self.name.clone(),
            kind,
            host: self.host.clone(),
            interval_s: self.interval_s.clamp(1, 3600) as u16,
            packets: self.packets.clamp(1, 10) as u8,
            timeout_ms: self.timeout_ms.clamp(100, 10_000) as u16,
        }
        .sanitize()
    }
}

// ---------------------------------------------------------------------------
// 账单 / 流量 / 套餐（M5）
// ---------------------------------------------------------------------------

/// 后台提交的账单。`price <= 0` 与非法周期在 API 层拦截。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BillingInput {
    pub price: f64,
    pub currency: String,
    /// monthly|quarterly|semiannual|annual|biennial|triennial|onetime|custom
    pub cycle: String,
    #[serde(default)]
    pub custom_cycle_days: Option<i64>,
    #[serde(default)]
    pub cycle_start_at: Option<i64>,
    #[serde(default)]
    pub expire_at: Option<i64>,
    #[serde(default)]
    pub auto_renew: bool,
    #[serde(default)]
    pub purchased_at: Option<i64>,
    #[serde(default)]
    pub remark: Option<String>,
}

/// 当期流量累计。
#[derive(Debug, Clone, Default)]
pub struct TrafficRow {
    pub server_id: ServerId,
    pub period_start: i64,
    pub in_bytes: i64,
    pub out_bytes: i64,
    /// 上次观测到的网卡累计值。`None` = 还没建基线
    pub last_raw_in: Option<i64>,
    pub last_raw_out: Option<i64>,
    /// 本周期已经报过的告警档位
    pub alerted_pct: Vec<u8>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TrafficConfig {
    /// `None` = 无限，前端显示 ♾️
    #[serde(default)]
    pub limit_bytes: Option<i64>,
    /// sum|max|min|upload|download
    #[serde(default = "default_calc_mode")]
    pub calc_mode: String,
    /// 1-31，31 表示当月最后一天
    #[serde(default = "default_reset_day")]
    pub reset_day: u32,
    /// 覆盖面板时区
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default = "default_alert_pct")]
    pub alert_pct: Vec<u8>,
}

fn default_calc_mode() -> String {
    "sum".into()
}
fn default_reset_day() -> u32 {
    1
}
fn default_alert_pct() -> Vec<u8> {
    vec![80, 95, 100]
}

impl Default for TrafficConfig {
    fn default() -> Self {
        Self {
            limit_bytes: None,
            calc_mode: default_calc_mode(),
            reset_day: default_reset_day(),
            timezone: None,
            alert_pct: default_alert_pct(),
        }
    }
}

/// 套餐模板（R16）：多台同款机器共享一条购买/测评链接。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlanRow {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub buy_url: Option<String>,
    #[serde(default)]
    pub review_url: Option<String>,
    #[serde(default)]
    pub price: Option<f64>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub cycle: Option<String>,
    #[serde(default)]
    pub created_at: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GroupRow {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub sort_order: i64,
}

// ---------------------------------------------------------------------------
// 通知（M6）
// ---------------------------------------------------------------------------

/// 渠道记录。`config` 从数据库读出时已解密。
#[derive(Debug, Clone)]
pub struct ChannelRow {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub config: crate::notify::ChannelConfig,
    pub enabled: bool,
}

/// 后台提交的渠道。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ChannelInput {
    pub name: String,
    #[serde(flatten)]
    pub config: crate::notify::ChannelConfig,
    #[serde(default = "yes")]
    pub enabled: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RuleRow {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    /// 事件类型字符串
    pub event_kinds: Vec<String>,
    pub channel_ids: Vec<i64>,
    #[serde(default)]
    pub scope_kind: Option<String>,
    #[serde(default)]
    pub scope_ids: Option<Vec<i64>>,
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default = "default_duration")]
    pub duration_s: i64,
    #[serde(default = "default_cooldown")]
    pub cooldown_s: i64,
    #[serde(default = "yes")]
    pub notify_resolve: bool,
    #[serde(default)]
    pub title_tpl: Option<String>,
    #[serde(default)]
    pub body_tpl: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn default_duration() -> i64 {
    90
}
fn default_cooldown() -> i64 {
    3600
}

impl RuleRow {
    pub fn scope(&self) -> PingScope {
        match self.scope_kind.as_deref() {
            Some(k) => PingScope::from_columns(
                k,
                &serde_json::to_string(self.scope_ids.as_deref().unwrap_or(&[]))
                    .unwrap_or_else(|_| "[]".into()),
            ),
            None => PingScope::All,
        }
    }

    pub fn rule_params(&self) -> crate::domain::alert::RuleParams {
        crate::domain::alert::RuleParams {
            duration_s: self.duration_s.clamp(0, 86_400),
            cooldown_s: self.cooldown_s.clamp(0, 30 * 86_400),
            notify_resolve: self.notify_resolve,
        }
    }
}

/// 告警历史的一行（含活跃与已恢复）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AlertRow {
    pub id: i64,
    pub rule_id: i64,
    pub server_id: Option<ServerId>,
    pub event_kind: String,
    pub state: String,
    pub first_at: i64,
    pub fired_at: Option<i64>,
    pub resolved_at: Option<i64>,
    pub last_notified_at: Option<i64>,
    pub payload: Option<String>,
}

// ---------------------------------------------------------------------------
// trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait Storage: Send + Sync + 'static {
    // ── 管理员 ──
    async fn count_admins(&self) -> Result<i64>;
    async fn create_admin(&self, username: &str, password_hash: &str, now: i64) -> Result<i64>;
    /// 只在**一个管理员都还没有**时创建，否则返回 `None`。
    ///
    /// 首次访问面板自行设定账号的入口靠它兜底：先查 count 再 INSERT 是
    /// TOCTOU —— 两个请求可以都读到 0、然后各建一个不同用户名的管理员。
    /// 所以判空和写入必须在**同一条语句**里完成。
    async fn create_first_admin(
        &self,
        username: &str,
        password_hash: &str,
        now: i64,
    ) -> Result<Option<i64>>;
    async fn get_admin(&self, username: &str) -> Result<Option<AdminUser>>;
    async fn touch_admin_login(&self, id: i64, now: i64) -> Result<()>;

    // ── refresh token 吊销 ──
    async fn revoke_token(&self, jti_hash: &str, expires_at: i64) -> Result<()>;
    async fn is_revoked(&self, jti_hash: &str) -> Result<bool>;
    /// 清掉已经过了原始有效期的吊销记录 —— 它们再也不会被查到了
    async fn sweep_revoked(&self, now: i64) -> Result<u64>;

    // ── 服务器管理 ──
    async fn create_server(&self, name: &str, token_hash: &str, now: i64) -> Result<ServerRecord>;
    async fn list_servers(&self) -> Result<Vec<ServerRecord>>;
    async fn get_server(&self, id: ServerId) -> Result<Option<ServerRecord>>;
    /// agent 认证的唯一入口：拿 token 的 SHA-256 反查服务器。
    /// 查不到就是凭据无效，直接 401。
    async fn get_server_by_token(&self, token_hash: &str) -> Result<Option<ServerRecord>>;
    /// 按对外 uuid 查。
    ///
    /// 内存里的 `AppState` 只装**本进程见过的会话**，所以不能拿它当身份来源：
    /// 面板刚重启、或者机器从没装过探针时，那里查不到 —— 但历史数据、账单、
    /// 名字都在库里。详情页的接口靠这个回落。
    async fn get_server_by_uuid(&self, uuid: &str) -> Result<Option<ServerRecord>>;
    async fn update_server(&self, id: ServerId, patch: &ServerPatch) -> Result<bool>;
    async fn delete_server(&self, id: ServerId) -> Result<bool>;
    /// 吊销旧凭据并换成新的
    async fn set_server_token(&self, id: ServerId, token_hash: &str) -> Result<bool>;
    async fn reorder_servers(&self, order: &[(ServerId, i64)]) -> Result<()>;
    /// agent 连上来时把 Hello 的静态信息写回
    async fn update_server_facts(&self, id: ServerId, f: &ServerFacts, now: i64) -> Result<()>;

    // ── 运行期配置 ──
    async fn get_runtime_config(&self, id: ServerId) -> Result<pulse_proto::RuntimeConfig>;
    async fn set_runtime_config(
        &self,
        id: ServerId,
        cfg: &pulse_proto::RuntimeConfig,
        now: i64,
    ) -> Result<()>;

    // ── 延迟监控任务 ──
    async fn create_ping_task(&self, t: &PingTaskInput, now: i64) -> Result<PingTaskRow>;
    async fn list_ping_tasks(&self) -> Result<Vec<PingTaskRow>>;
    async fn update_ping_task(&self, id: i64, t: &PingTaskInput) -> Result<bool>;
    async fn delete_ping_task(&self, id: i64) -> Result<bool>;
    /// 解析作用范围，返回这台机器该跑的任务。只包含 enabled 的。
    async fn ping_tasks_for_server(&self, id: ServerId) -> Result<Vec<pulse_proto::PingTaskSpec>>;

    // ── 账单 ──
    async fn get_billing(&self, id: ServerId) -> Result<Option<crate::domain::billing::Billing>>;
    async fn set_billing(&self, id: ServerId, b: &BillingInput, now: i64) -> Result<()>;
    async fn delete_billing(&self, id: ServerId) -> Result<bool>;
    /// 全部机器的账单，用于汇总。`None` 表示这台没填账单。
    /// 带上 uuid / name / country_code：这些是**从没连过 agent 的机器**
    /// 唯一的身份来源。
    async fn all_billing(&self) -> Result<Vec<BillingEntry>>;

    // ── 流量 ──
    async fn get_traffic(&self, id: ServerId) -> Result<Option<TrafficRow>>;
    async fn upsert_traffic(&self, row: &TrafficRow, now: i64) -> Result<()>;
    /// 把一个已结束的周期归档。幂等：主键是 (server_id, period_start)。
    async fn archive_traffic(
        &self,
        id: ServerId,
        period_start: i64,
        period_end: i64,
        in_bytes: i64,
        out_bytes: i64,
    ) -> Result<()>;
    async fn get_traffic_config(&self, id: ServerId) -> Result<TrafficConfig>;
    async fn set_traffic_config(&self, id: ServerId, c: &TrafficConfig) -> Result<()>;

    // ── 套餐与分组 ──
    async fn list_plans(&self) -> Result<Vec<PlanRow>>;
    async fn create_plan(&self, p: &PlanRow, now: i64) -> Result<PlanRow>;
    async fn update_plan(&self, id: i64, p: &PlanRow) -> Result<bool>;
    async fn delete_plan(&self, id: i64) -> Result<bool>;
    async fn list_groups(&self) -> Result<Vec<GroupRow>>;
    async fn create_group(&self, g: &GroupRow, now: i64) -> Result<GroupRow>;
    async fn update_group(&self, id: i64, g: &GroupRow) -> Result<bool>;
    async fn delete_group(&self, id: i64) -> Result<bool>;

    // ── 汇率 ──
    async fn load_rates(&self) -> Result<crate::domain::rate::Rates>;
    async fn save_rates(&self, r: &crate::domain::rate::Rates, now: i64) -> Result<()>;
    async fn set_manual_rate(&self, quote: &str, rate: f64, now: i64) -> Result<()>;
    async fn delete_manual_rate(&self, quote: &str) -> Result<bool>;

    // ── 通知渠道 ──
    /// `secret` 用于解密凭据。解不开的渠道会被跳过并记日志，
    /// 而不是让整个列表失败。
    async fn list_channels(&self, secret: &[u8]) -> Result<Vec<ChannelRow>>;
    async fn create_channel(&self, c: &ChannelInput, secret: &[u8], now: i64) -> Result<i64>;
    async fn update_channel(&self, id: i64, c: &ChannelInput, secret: &[u8]) -> Result<bool>;
    async fn delete_channel(&self, id: i64) -> Result<bool>;

    // ── 通知规则 ──
    async fn list_rules(&self) -> Result<Vec<RuleRow>>;
    async fn create_rule(&self, r: &RuleRow, now: i64) -> Result<i64>;
    async fn update_rule(&self, id: i64, r: &RuleRow) -> Result<bool>;
    async fn delete_rule(&self, id: i64) -> Result<bool>;

    // ── 告警状态机 ──
    /// 读一条活跃记录（`state != 'resolved'`）。
    async fn get_alert(
        &self,
        rule_id: i64,
        server_id: Option<ServerId>,
        kind: &str,
    ) -> Result<Option<crate::domain::alert::AlertEvent>>;
    async fn upsert_alert(
        &self,
        rule_id: i64,
        server_id: Option<ServerId>,
        kind: &str,
        e: &crate::domain::alert::AlertEvent,
        payload: Option<&str>,
    ) -> Result<()>;
    /// 状态机判定这条记录可以清掉时调用。
    async fn clear_alert(
        &self,
        rule_id: i64,
        server_id: Option<ServerId>,
        kind: &str,
    ) -> Result<()>;
    /// 告警历史，最新在前。
    async fn list_alerts(&self, limit: i64) -> Result<Vec<AlertRow>>;

    // ── 站点设置 ──
    async fn get_setting(&self, key: &str) -> Result<Option<String>>;
    async fn set_setting(&self, key: &str, value: &str, now: i64) -> Result<()>;

    /// 只有批量写。单行独立事务是 SQLite 慢的根源。
    async fn insert_metrics(&self, layer: MetricLayer, rows: &[MetricRow]) -> Result<u64>;
    async fn insert_ping(&self, layer: PingLayer, rows: &[PingRow]) -> Result<u64>;

    async fn query_metrics(&self, server: ServerId, from: i64, to: i64) -> Result<MetricSeries>;

    async fn query_ping(
        &self,
        server: ServerId,
        task: i64,
        from: i64,
        to: i64,
    ) -> Result<PingSeries>;

    /// 把已完成的时间窗上卷到更粗的层。幂等：重复执行结果相同。
    ///
    /// **每轮处理的窗口有上限**，落后很多时不会变成一个巨型事务 ——
    /// 那会长时间独占写连接，把 agent 的指标上报堵住。
    /// 返回的 `caught_up` 为 false 时，调用方应立即再跑一轮。
    async fn rollup(&self, now: i64) -> Result<RollupReport>;

    /// 分块删除过期行。长事务会让 WAL 暴涨并阻塞写者，所以必须分块。
    async fn prune(&self, policy: &RetentionPolicy, now: i64) -> Result<PruneReport>;

    async fn stats(&self) -> Result<StorageStats>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_scope_covers_the_right_servers() {
        assert!(PingScope::All.covers(1, None));
        assert!(PingScope::All.covers(99, Some(3)));

        let g = PingScope::Group(vec![2, 3]);
        assert!(g.covers(1, Some(2)));
        assert!(!g.covers(1, Some(9)));
        assert!(!g.covers(1, None), "未分组的机器不该被分组范围覆盖");

        let s = PingScope::Servers(vec![7, 8]);
        assert!(s.covers(7, None));
        assert!(!s.covers(9, Some(2)));
    }

    #[test]
    fn ping_scope_survives_corrupt_columns() {
        // 数据被改坏时宁可多探也不要静默地什么都不探
        assert_eq!(PingScope::from_columns("bogus", "[]"), PingScope::All);
        assert_eq!(
            PingScope::from_columns("group", "not-json"),
            PingScope::Group(vec![])
        );
        let (k, ids) = PingScope::Group(vec![1, 2]).to_columns();
        assert_eq!((k, ids.as_str()), ("group", "[1,2]"));
        assert_eq!(
            PingScope::from_columns(k, &ids),
            PingScope::Group(vec![1, 2])
        );
    }

    #[test]
    fn ping_row_to_spec_clamps_and_falls_back() {
        use pulse_proto::PingKind;
        let row = |kind: &str, port, interval, packets| PingTaskRow {
            id: 1,
            name: "t".into(),
            kind: kind.into(),
            host: "h".into(),
            port,
            expect_status: None,
            interval_s: interval,
            packets,
            timeout_ms: 3000,
            scope: PingScope::All,
            enabled: true,
            created_at: 0,
        };
        assert_eq!(
            row("tcp", Some(80), 60, 3).to_spec().kind,
            PingKind::Tcp { port: 80 }
        );
        assert_eq!(row("icmp", None, 60, 3).to_spec().kind, PingKind::Icmp);
        // 非法 kind / 缺 port 时回落 TCP:443，而不是让整条任务失效
        assert_eq!(
            row("garbage", None, 60, 3).to_spec().kind,
            PingKind::Tcp { port: 443 }
        );
        // 数值夹紧
        let s = row("tcp", Some(80), 1, 100).to_spec();
        assert_eq!(s.interval_s, pulse_proto::PingTaskSpec::MIN_INTERVAL_S);
        assert_eq!(s.packets, pulse_proto::PingTaskSpec::MAX_PACKETS);
    }

    #[test]
    fn metric_cols_match_row_values() {
        // 本模块最容易出的静默 bug：列名数组和取值数组长度不一致时，
        // 值会整体错位塞进相邻列，而 SQL 完全不会报错。
        assert_eq!(METRIC_COLS.len(), METRIC_COL_COUNT);
        assert_eq!(MetricRow::default().values().len(), METRIC_COL_COUNT);
    }

    #[test]
    fn metric_cols_have_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for (c, _) in METRIC_COLS {
            assert!(seen.insert(*c), "列名重复: {c}");
        }
    }

    #[test]
    fn values_from_values_roundtrip() {
        // from_values 的下标写错一个就会让两列互换，且不会有任何报错。
        // 用「每列一个唯一值」的行做往返，任何错位都会被抓到。
        let v: [Option<i64>; METRIC_COL_COUNT] = std::array::from_fn(|i| Some(i as i64 + 1000));
        let row = MetricRow::from_values(7, 42, v);
        assert_eq!(row.server_id, 7);
        assert_eq!(row.ts, 42);
        assert_eq!(row.values(), v, "values 与 from_values 的列顺序不一致");
    }

    #[test]
    fn aggregate_uses_avg_and_max_per_column() {
        let mk = |cpu, peak| MetricRow {
            cpu_pct: Some(cpu),
            net_in_peak: Some(peak),
            ..Default::default()
        };
        let out = aggregate_window(&[mk(100, 5), mk(200, 50), mk(300, 20)], 1, 60);
        assert_eq!(out.cpu_pct, Some(200), "cpu_pct 应取平均");
        assert_eq!(out.net_in_peak, Some(50), "net_in_peak 应取最大");
        assert_eq!(out.server_id, 1);
        assert_eq!(out.ts, 60);
    }

    #[test]
    fn aggregate_returns_none_not_zero_for_missing_columns() {
        // 用 0 表示「没数据」会让前端画出一条假的零线
        let out = aggregate_window(
            &[MetricRow {
                cpu_pct: Some(1),
                ..Default::default()
            }],
            1,
            60,
        );
        assert_eq!(out.cpu_pct, Some(1));
        assert_eq!(out.gpu_util, None, "未采集的列必须是 None 而不是 0");
        assert_eq!(out.cpu_temp, None);
    }

    #[test]
    fn aggregate_of_empty_window_is_all_none() {
        let out = aggregate_window(&[], 1, 60);
        assert!(out.values().iter().all(Option::is_none));
    }

    #[test]
    fn aggregate_does_not_overflow_on_large_values() {
        // 网卡累计计数可以逼近 u64 上限；用 i128 中间和避免求平均时溢出
        let big = i64::MAX / 2;
        let rows: Vec<_> = (0..8)
            .map(|_| MetricRow {
                mem_total: Some(big),
                ..Default::default()
            })
            .collect();
        assert_eq!(aggregate_window(&rows, 1, 0).mem_total, Some(big));
    }

    #[test]
    fn query_plan_keeps_points_bounded() {
        // 表格来自 ：任何跨度都必须落在 MAX_POINTS 之内
        for (label, range) in [
            ("1h", 3_600i64),
            ("6h", 21_600),
            ("24h", 86_400),
            ("7d", 604_800),
            ("30d", 2_592_000),
            ("90d", 7_776_000),
            ("1y", 31_536_000),
            ("5y", 157_680_000),
        ] {
            let p = plan_query(range);
            let points = range / p.step;
            assert!(
                points <= MAX_POINTS,
                "{label}: {points} 点超过上限 {MAX_POINTS}（step={}）",
                p.step
            );
            assert_eq!(
                p.step % p.granularity.base_step(),
                0,
                "{label}: step 必须是该层步长的整数倍"
            );
        }
    }

    #[test]
    fn query_plan_picks_layer_by_range() {
        assert_eq!(plan_query(21_600).granularity, Granularity::Minute); // 6h
        assert_eq!(plan_query(604_800).granularity, Granularity::Minute); // 7d 边界内
        assert_eq!(plan_query(604_801).granularity, Granularity::Hour); // 刚过 7d
        assert_eq!(plan_query(2_592_000).granularity, Granularity::Hour); // 30d
    }

    #[test]
    fn query_plan_does_not_upsample() {
        // 短跨度不能返回比原始数据更细的步长
        let p = plan_query(600); // 10 分钟
        assert_eq!(p.step, 60, "分钟层最细就是 60 秒");
        assert!(p.step >= Granularity::Minute.base_step());
    }

    #[test]
    fn ceil_div_rounds_up() {
        assert_eq!(ceil_div(1, 750), 1);
        assert_eq!(ceil_div(750, 750), 1);
        assert_eq!(ceil_div(751, 750), 2);
        assert_eq!(ceil_div(1500, 750), 2);
        assert_eq!(ceil_div(1501, 750), 3);
    }

    #[test]
    fn query_plan_handles_degenerate_ranges() {
        // 0 或负数范围不能 panic、不能除零
        for r in [0i64, -1, 1] {
            let p = plan_query(r);
            assert!(p.step >= 60);
        }
    }
}
