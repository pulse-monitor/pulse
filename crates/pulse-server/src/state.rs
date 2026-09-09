//! 内存实时层。
//!
//! 按 的路径 A：2 秒粒度的数据**不落盘**，
//! 只活在每台机器的环形缓冲里。数据库里最细的粒度是 1 分钟。
//! 这一条决策消灭了约 99% 的写入压力 —— 对照数据

use std::collections::VecDeque;
use std::sync::Arc;

use dashmap::DashMap;
use pulse_proto::{Capabilities, Hello, Metrics, RuntimeConfig, ServerMsg};
use serde::Serialize;
use tokio::sync::mpsc;

use crate::store::{aggregate_window, MetricRow, ServerId};

/// 每台机器保留的实时采样点数。
///
/// 2 秒间隔下约等于 10 分钟历史，足够前端画"实时曲线"。
/// 内存占用：约 368 B/点 × 300 × 200 台 ≈ 22 MiB。
pub const RING_CAP: usize = 300;
/// 卡片走势图保留多少个延迟点。20 个点在 100px 宽的图上正好一点一像素带间隔，
/// 再多也看不出来，而每台机器多存一个点就是 200 个 f32。
const SPARK_CAP: usize = 20;

/// 离线判定的宽限：`interval × 3 + 10s`。M0/M1 的 interval 为 2 秒。
const OFFLINE_GRACE_S: i64 = 2 * 3 + 10;

#[derive(Debug)]
pub struct ServerEntry {
    /// 对外标识（由 token 派生），不是数据库主键
    pub uuid: String,
    /// 数据库主键。首次 upsert 之前为 None，此时指标只进内存不落盘。
    pub db_id: Option<ServerId>,
    pub hello: Option<Hello>,
    pub last_seen: i64,
    /// WS 连接是否还在。断开即刻置 false —— 不等超时。
    pub connected: bool,
    /// 实时环形缓冲，按 ts 递增
    pub ring: VecDeque<MetricRow>,
    /// 最近一条原始上报。环形缓冲里存的是存储行（丢掉了网卡名等非数值字段），
    /// 而前端要显示网卡列表与 GPU 型号，所以原始消息也留一份最新的。
    pub last: Option<Metrics>,
    /// 后台设定的显示名，随 servers 表同步进来。
    ///
    /// **必须以它为准，不能用 agent 上报的 hostname** —— 用户在后台特意
    /// 起了名字，前端却显示 `jqwebs.evo.host.aliceinit.dev` 是明显的 bug。
    /// hostname 只在这台机器还没和数据库对上时兜底。
    pub name: Option<String>,
    /// 由 GeoIP 或人工设置的国家代码，随 servers 表同步进来
    pub country_code: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub group_id: Option<i64>,
    /// 购买同款 / 测评链接（R16 的最后两个字段）
    pub buy_url: Option<String>,
    pub review_url: Option<String>,
    /// 最近一次延迟探测结果
    pub latency: Option<LatencyView>,
    /// 最近若干次延迟与丢包，供卡片上的迷你走势图用。
    ///
    /// 只放内存、有界（[`SPARK_CAP`]）—— 卡片上那条走势图不值得为它查一次库，
    /// 200 台机器每次刷新都查一遍历史会把读连接吃光。
    pub spark: VecDeque<(f32, f32)>,
    /// 通往该 agent 的 WS 发送端。断开时置 None。
    ///
    /// 有了它，后台改配置才能**立刻推下去**而不是等 agent 下次重连 ——
    /// 这就是 里「运行期选项改完 ≤2 秒生效」的实现。
    pub tx: Option<mpsc::UnboundedSender<ServerMsg>>,
}

impl ServerEntry {
    fn new(uuid: String) -> Self {
        Self {
            uuid,
            db_id: None,
            hello: None,
            last_seen: 0,
            connected: false,
            ring: VecDeque::with_capacity(RING_CAP),
            last: None,
            name: None,
            country_code: None,
            latitude: None,
            longitude: None,
            group_id: None,
            buy_url: None,
            review_url: None,
            latency: None,
            spark: VecDeque::with_capacity(SPARK_CAP),
            tx: None,
        }
    }

    fn online(&self, now: i64) -> bool {
        self.connected && (now - self.last_seen) <= OFFLINE_GRACE_S
    }

    fn push(&mut self, row: MetricRow) {
        if self.ring.len() == RING_CAP {
            self.ring.pop_front();
        }
        self.ring.push_back(row);
    }
}

/// 对外的服务器视图。**不包含 token** —— `id` 是派生标识，
/// 公开接口绝不能回显凭据本身。
/// `servers` 表里那些前端要用、但不来自 agent 的字段。
///
/// 收成一个结构体而不是七个参数：加字段时不用改所有调用点，
/// 也不会出现「两个 Option<String> 传反了」这种编译器抓不到的错。
#[derive(Debug, Clone, Default)]
pub struct ServerMeta {
    pub name: Option<String>,
    pub country_code: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub group_id: Option<i64>,
    pub buy_url: Option<String>,
    pub review_url: Option<String>,
}

impl From<&crate::store::ServerRecord> for ServerMeta {
    fn from(r: &crate::store::ServerRecord) -> Self {
        Self {
            name: Some(r.name.clone()),
            country_code: r.country_code.clone(),
            latitude: r.latitude,
            longitude: r.longitude,
            group_id: r.group_id,
            buy_url: r.buy_url.clone(),
            review_url: r.review_url.clone(),
        }
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct PublicServer {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub os: String,
    pub kernel: Option<String>,
    pub arch: String,
    pub virtualization: Option<String>,
    /// ISO 3166-1 alpha-2，前端转成国旗 emoji
    pub country_code: Option<String>,
    /// 经纬度，供 3D 地球打点。为空时前端用国家坐标回落
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// 所属分组。前端按它分区展示（R11）
    pub group_id: Option<i64>,
    /// 购买同款 / 测评链接（R16）。为空时卡片上不显示那个按钮
    pub buy_url: Option<String>,
    pub review_url: Option<String>,
    pub cpu_model: Option<String>,
    pub cpu_cores: u32,
    pub uptime_s: i64,
    pub last_seen: i64,
    pub agent_version: String,

    pub cpu_pct: f32,
    /// 1 / 5 / 15 分钟负载。`capabilities.load_average == false` 时前端应隐藏。
    /// 三个都给：卡片上并排显示 `0.04, 0.01, 0.00` 比只有一个数更能看出趋势。
    pub load1: Option<f32>,
    pub load5: Option<f32>,
    pub load15: Option<f32>,
    pub mem: MemView,
    pub disk: DiskView,
    pub net: NetView,
    pub tcp_conn: Option<u32>,
    pub proc_count: Option<u32>,
    /// 摄氏度
    pub cpu_temp: Option<f32>,
    pub gpu: Option<GpuView>,
    /// 最近一次延迟探测。未配置监控任务时为 None，前端隐藏该行
    pub latency: Option<LatencyView>,
    /// 最近若干次延迟 / 丢包，供卡片上的迷你走势图。空数组 = 还没探测过。
    pub latency_spark: Vec<f32>,
    pub loss_spark: Vec<f32>,

    /// 本机实际能采到什么。前端据此**隐藏**字段 —— 显示 0 是撒谎。
    pub capabilities: Capabilities,
}

/// 内存视图。原始三元组一并给出，因为「含不含 buff/cache」是展示期选项，
/// 前端切换开关时不需要重新请求。
#[derive(Debug, Default, Clone, Serialize)]
pub struct MemView {
    pub total: u64,
    pub free: u64,
    pub available: u64,
    /// 不含 buff/cache 的口径：total - available
    pub used: u64,
    /// 含 buff/cache 的口径：total - free
    pub used_with_cache: u64,
    pub pct: f32,
    pub swap_total: u64,
    pub swap_used: u64,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct DiskView {
    pub total: u64,
    pub used: u64,
    pub pct: f32,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct NetView {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_speed: u64,
    pub tx_speed: u64,
    pub ifaces: Vec<String>,
}

/// 最近一次延迟探测的结果。
///
/// M4 把探测结果直接落库了（它本来就是分钟粒度），但服务器卡片要显示
/// 延迟/丢包（R16），而首页不允许查库 —— 所以在结果到达时顺手在内存里留一份。
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct LatencyView {
    pub task_id: i64,
    pub rtt_ms: f64,
    pub loss_pct: f64,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct GpuView {
    pub util: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub temp: f32,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub total: usize,
    pub online: usize,
    pub offline: usize,
}

// ---------------------------------------------------------------------------
// 账单 / 流量的内存视图
//
// 这些数据低频变化，由后台任务定期刷进内存 —— 这样首页 summary
// 保持「0 次数据库查询」（M1 定下的约束）。
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct BillingView {
    pub price: f64,
    pub currency: String,
    pub cycle: &'static str,
    pub expire_at: Option<i64>,
    pub auto_renew: bool,
    /// 剩余天数。负数 = 已过期；`None` = 买断或未设到期时间
    pub remain_days: Option<i64>,
    /// 原币种的剩余价值
    pub remaining_value: f64,
    /// 买断机器：剩余时间是 ♾️
    pub infinite: bool,
    /// 未设到期时间 —— 前端显示「—」而不是「0 天」
    pub unknown_expiry: bool,
    /// 换算到展示币种。`None` = 该货币没有汇率
    pub price_display: Option<f64>,
    pub remaining_display: Option<f64>,
    pub rate_level: crate::domain::rate::Level,
    pub renew_state: crate::domain::billing::RenewState,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrafficView {
    pub in_bytes: u64,
    pub out_bytes: u64,
    /// 按 `calc_mode` 算出的已用量
    pub used: u64,
    /// `None` = 无限，前端显示 ♾️
    pub limit: Option<u64>,
    /// `None` = 无限流量，不该显示进度条
    pub pct: Option<f64>,
    pub calc_mode: &'static str,
    pub period_start: i64,
    pub reset_day: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerValue {
    pub server_id: ServerId,
    /// 对外标识与名字**取自数据库**而不是内存中的 agent 状态 ——
    /// 一台刚建好、还没装 agent 的机器，它的账单与到期时间照样要能显示。
    pub uuid: String,
    pub name: String,
    pub country_code: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub group_id: Option<i64>,
    pub buy_url: Option<String>,
    pub review_url: Option<String>,
    /// 不在公开页展示
    pub hidden: bool,
    /// agent 上报过的静态信息。从没上线的机器为 None
    pub os: Option<String>,
    pub arch: Option<String>,
    pub cpu_cores: Option<i64>,
    pub mem_total: Option<i64>,
    pub disk_total: Option<i64>,
    pub agent_version: Option<String>,
    pub last_seen_at: Option<i64>,
    pub billing: Option<BillingView>,
    pub traffic: TrafficView,
}

impl ServerValue {
    /// 为「数据库里有、但从没连过 agent」的机器造一个离线视图。
    ///
    /// 一台刚添加、还没装探针的机器**必须在面板上看得见** ——
    /// 那正是你最想确认它状态的时候。
    pub fn offline_view(&self) -> PublicServer {
        PublicServer {
            id: self.uuid.clone(),
            name: self.name.clone(),
            online: false,
            os: self.os.clone().unwrap_or_default(),
            arch: self.arch.clone().unwrap_or_default(),
            country_code: self.country_code.clone(),
            buy_url: self.buy_url.clone(),
            review_url: self.review_url.clone(),
            latitude: self.latitude,
            longitude: self.longitude,
            group_id: self.group_id,
            cpu_cores: self.cpu_cores.unwrap_or(0) as u32,
            mem: MemView {
                total: self.mem_total.unwrap_or(0) as u64,
                ..Default::default()
            },
            disk: DiskView {
                total: self.disk_total.unwrap_or(0) as u64,
                ..Default::default()
            },
            agent_version: self.agent_version.clone().unwrap_or_default(),
            last_seen: self.last_seen_at.unwrap_or(0),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ValueSnapshot {
    pub servers: Vec<ServerValue>,
    pub totals: crate::domain::billing::Totals,
    pub display_currency: String,
    pub rate_as_of: String,
    pub rate_fetched_at: i64,
    pub rate_stale: bool,
    pub updated_at: i64,
}

/// 用 DashMap 而不是全局 `RwLock<HashMap>`：flush 任务每 60 秒要遍历全部机器，
/// 期间 200 个 agent 还在持续写入。分片锁避免了写者被遍历饿死。
#[derive(Clone, Default)]
pub struct AppState {
    inner: Arc<DashMap<String, ServerEntry>>,
    /// 账单/流量的缓存快照。读多写少，用 `RwLock` 就够
    values: Arc<std::sync::RwLock<ValueSnapshot>>,
}

impl AppState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_connect(&self, uuid: &str, tx: mpsc::UnboundedSender<ServerMsg>) {
        let mut e = self
            .inner
            .entry(uuid.to_string())
            .or_insert_with(|| ServerEntry::new(uuid.to_string()));
        e.connected = true;
        e.tx = Some(tx);
        e.last_seen = now_unix();
    }

    pub fn on_disconnect(&self, uuid: &str) {
        if let Some(mut e) = self.inner.get_mut(uuid) {
            e.connected = false;
            // 丢掉发送端，否则后台改配置时会往一条死连接里写
            e.tx = None;
        }
    }

    /// 把新配置立刻推给在线的 agent。返回是否真的送出去了。
    ///
    /// 送不出去不是错误 —— 机器可能正好离线，重连时 agent 会重新拉一次配置。
    pub fn push_config(&self, uuid: &str, cfg: RuntimeConfig) -> bool {
        self.push(uuid, ServerMsg::Config(cfg))
    }

    /// 把探测任务列表推给在线的 agent。全量替换语义。
    pub fn push_tasks(&self, uuid: &str, tasks: Vec<pulse_proto::PingTaskSpec>) -> bool {
        self.push(uuid, ServerMsg::PingTasks { tasks })
    }

    /// 让一台 agent 升级到某个版本。**只发版本号** —— 下载地址由 agent
    /// 本地配置决定，面板即使被攻陷也指不了源。
    pub fn push_upgrade(&self, uuid: &str, version: String) -> bool {
        self.push(uuid, ServerMsg::Upgrade { version })
    }

    fn push(&self, uuid: &str, msg: ServerMsg) -> bool {
        self.inner
            .get(uuid)
            .and_then(|e| e.tx.clone())
            .is_some_and(|tx| tx.send(msg).is_ok())
    }

    pub fn set_values(&self, v: ValueSnapshot) {
        if let Ok(mut w) = self.values.write() {
            *w = v;
        }
    }

    /// 全站实时总带宽 `(下行, 上行)` B/s。只算在线机器。
    pub fn total_speed(&self) -> (u64, u64) {
        let now = now_unix();
        self.inner
            .iter()
            .filter(|e| e.value().online(now))
            .filter_map(|e| {
                e.value()
                    .last
                    .as_ref()
                    .map(|m| (m.net.rx_speed, m.net.tx_speed))
            })
            .fold((0, 0), |(a, b), (x, y)| (a + x, b + y))
    }

    /// 读账单/流量快照。**不查数据库。**
    pub fn values(&self) -> ValueSnapshot {
        self.values.read().map(|v| v.clone()).unwrap_or_default()
    }

    /// 每台机器最新的网卡累计计数，供流量结算任务使用。
    ///
    /// 返回 `(db_id, rx_bytes, tx_bytes, ifaces)`。只包含**有过上报**的机器 ——
    /// 没上报过就没有基线可算。
    pub fn net_counters(&self) -> Vec<(ServerId, u64, u64, Vec<String>)> {
        self.inner
            .iter()
            .filter_map(|e| {
                let v = e.value();
                let id = v.db_id?;
                let m = v.last.as_ref()?;
                Some((id, m.net.rx_bytes, m.net.tx_bytes, m.net.ifaces.clone()))
            })
            .collect()
    }

    /// 当前在线的机器（uuid, db_id）。任务变更后要挨个重推。
    pub fn online_servers(&self) -> Vec<(String, ServerId)> {
        let now = now_unix();
        self.inner
            .iter()
            .filter(|e| e.value().online(now))
            .filter_map(|e| e.value().db_id.map(|id| (e.key().clone(), id)))
            .collect()
    }

    pub fn on_hello(&self, uuid: &str, hello: Hello) {
        let mut e = self
            .inner
            .entry(uuid.to_string())
            .or_insert_with(|| ServerEntry::new(uuid.to_string()));
        e.hello = Some(hello);
        e.last_seen = now_unix();
    }

    /// 记录最近一次延迟探测结果，供服务器卡片显示（R16）。
    pub fn on_ping(&self, uuid: &str, l: LatencyView) {
        if let Some(mut e) = self.inner.get_mut(uuid) {
            if e.spark.len() == SPARK_CAP {
                e.spark.pop_front();
            }
            e.spark.push_back((l.rtt_ms as f32, l.loss_pct as f32));
            e.latency = Some(l);
        }
    }

    /// 同步 servers 表里那些前端要用、但不来自 agent 的字段。
    pub fn set_meta(&self, uuid: &str, m: ServerMeta) {
        if let Some(mut e) = self.inner.get_mut(uuid) {
            e.name = m.name;
            e.country_code = m.country_code;
            e.latitude = m.latitude;
            e.longitude = m.longitude;
            e.group_id = m.group_id;
            e.buy_url = m.buy_url;
            e.review_url = m.review_url;
        }
    }

    /// 一台机器被删掉时，把内存里的条目一并去掉。
    ///
    /// 不清的话它会一直挂在公开列表里（显示成「离线」），
    /// 而数据库里已经没有这台机器了 —— 刷新页面都赶不走。
    pub fn forget(&self, uuid: &str) {
        self.inner.remove(uuid);
    }

    pub fn set_db_id(&self, uuid: &str, id: ServerId) {
        // 用 entry 而不是 get_mut：条目不存在时静默无操作是「错误不吞」守则
        // 明确禁止的模式 —— 那会让「机器不落盘」变成一个查不出原因的现象。
        let mut e = self
            .inner
            .entry(uuid.to_string())
            .or_insert_with(|| ServerEntry::new(uuid.to_string()));
        e.db_id = Some(id);
    }

    pub fn on_metrics(&self, uuid: &str, m: Metrics) {
        let mut e = self
            .inner
            .entry(uuid.to_string())
            .or_insert_with(|| ServerEntry::new(uuid.to_string()));
        let db_id = e.db_id.unwrap_or(0);
        e.push(to_row(db_id, &m));
        e.last = Some(m);
        e.last_seen = now_unix();
    }

    pub fn list(&self) -> Vec<PublicServer> {
        let now = now_unix();
        let mut v: Vec<_> = self
            .inner
            .iter()
            .map(|e| to_public(e.value(), now))
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// 全站统计。
    ///
    /// **总数以数据库为准**（价值缓存），在线数取自实时层 ——
    /// 只数内存的话，刚添加还没装探针的机器不会计入总数，
    /// 用户会以为自己少了一台。
    pub fn summary(&self) -> Summary {
        let now = now_unix();
        let online = self.inner.iter().filter(|e| e.value().online(now)).count();
        // 价值缓存首次刷新前回落到内存计数，避免启动瞬间显示 0 台
        let total = self
            .values()
            .servers
            .iter()
            .filter(|v| !v.hidden)
            .count()
            .max(self.inner.len());
        Summary {
            total,
            online,
            offline: total.saturating_sub(online),
        }
    }

    /// 查一台机器的数据库主键。
    pub fn db_id(&self, uuid: &str) -> Option<ServerId> {
        self.inner.get(uuid).and_then(|e| e.db_id)
    }

    /// 取出 `[minute_ts, minute_ts + 60)` 窗口内每台机器的聚合行，供 flush 任务落盘。
    ///
    /// **不从环形缓冲里移除** —— 缓冲同时是前端实时曲线的数据源，
    /// 让它按容量自然淘汰即可。落盘用 upsert，所以重复 flush 是幂等的。
    pub fn snapshot_minute(&self, minute_ts: i64) -> Vec<MetricRow> {
        let end = minute_ts + 60;
        let mut out = Vec::new();
        for e in self.inner.iter() {
            let Some(db_id) = e.db_id else { continue };
            let window: Vec<MetricRow> = e
                .ring
                .iter()
                .filter(|r| r.ts >= minute_ts && r.ts < end)
                .cloned()
                .collect();
            if window.is_empty() {
                continue;
            }
            out.push(aggregate_window(&window, db_id, minute_ts));
        }
        out
    }

    /// 实时曲线：直接读内存，**不查数据库**。
    pub fn realtime(&self, uuid: &str, since: i64) -> Vec<MetricRow> {
        self.inner
            .get(uuid)
            .map(|e| e.ring.iter().filter(|r| r.ts >= since).cloned().collect())
            .unwrap_or_default()
    }
}

/// 协议消息 → 存储行。
///
/// **拿不到的字段保持 `None` 而不是 0**：0 是个有意义的值，
/// 用它表示「没数据」会让前端画出一条假的零线，也会污染聚合平均。
/// agent 已经通过 `capabilities` 声明了哪些字段本机采不到。
fn to_row(server_id: ServerId, m: &Metrics) -> MetricRow {
    let some_u64 = |v: u64| (v > 0).then_some(v as i64);
    MetricRow {
        server_id,
        ts: m.ts,
        cpu_pct: Some(i64::from(m.cpu_pct)),
        cpu_pct_max: Some(i64::from(m.cpu_pct)),
        // load[0] 为 0 可能是「真的空闲」也可能是「本平台没有负载概念」，
        // 后者由 capabilities.load_average 区分，这里照实存
        load1: Some(i64::from(m.load[0])),
        mem_total: some_u64(m.mem.total),
        mem_free: some_u64(m.mem.free),
        mem_available: some_u64(m.mem.available),
        swap_used: some_u64(m.mem.swap_total.saturating_sub(m.mem.swap_free)),
        disk_used: some_u64(m.disk.used),
        disk_total: some_u64(m.disk.total),
        net_in_speed: Some(m.net.rx_speed as i64),
        net_out_speed: Some(m.net.tx_speed as i64),
        net_in_peak: Some(m.net.rx_speed as i64),
        net_out_peak: Some(m.net.tx_speed as i64),
        net_in_total: some_u64(m.net.rx_bytes),
        net_out_total: some_u64(m.net.tx_bytes),
        tcp_conn: m.tcp_conn.map(i64::from),
        udp_conn: m.udp_conn.map(i64::from),
        proc_count: m.proc_count.map(i64::from),
        gpu_util: m.gpu.map(|g| i64::from(g.util)),
        gpu_mem_used: m.gpu.map(|g| g.mem_used as i64),
        gpu_temp: m.gpu.map(|g| i64::from(g.temp)),
        cpu_temp: m.cpu_temp.map(i64::from),
    }
}

fn to_public(e: &ServerEntry, now: i64) -> PublicServer {
    let h = e.hello.as_ref();
    let caps = h.map(|h| h.capabilities.clone()).unwrap_or_default();
    let m = e.last.as_ref();

    let mem = m
        .map(|m| {
            let t = m.mem.total;
            // 两种口径都算好给前端，切换开关时不用重新请求
            let used = crate::domain::mem::used(t, m.mem.free, m.mem.available, false);
            MemView {
                total: t,
                free: m.mem.free,
                available: m.mem.available,
                used,
                used_with_cache: crate::domain::mem::used(t, m.mem.free, m.mem.available, true),
                pct: crate::domain::mem::pct(used, t).unwrap_or(0.0),
                swap_total: m.mem.swap_total,
                swap_used: m.mem.swap_total.saturating_sub(m.mem.swap_free),
            }
        })
        .unwrap_or_default();

    let disk = m
        .map(|m| DiskView {
            total: m.disk.total,
            used: m.disk.used,
            pct: pct(m.disk.used, m.disk.total),
        })
        .unwrap_or_default();

    PublicServer {
        id: e.uuid.clone(),
        // 后台设定的名字优先。hostname 只在还没和数据库对上时兜底
        name: e
            .name
            .clone()
            .or_else(|| h.map(|h| h.hostname.clone()))
            .unwrap_or_else(|| "(未上报)".into()),
        online: e.online(now),
        os: h.map(|h| h.os.clone()).unwrap_or_default(),
        kernel: h.and_then(|h| h.kernel.clone()),
        arch: h.map(|h| h.arch.clone()).unwrap_or_default(),
        virtualization: h.and_then(|h| h.virtualization.clone()),
        country_code: e.country_code.clone(),
        latitude: e.latitude,
        longitude: e.longitude,
        group_id: e.group_id,
        buy_url: e.buy_url.clone(),
        review_url: e.review_url.clone(),
        cpu_model: h.and_then(|h| h.cpu_model.clone()),
        cpu_cores: h.map(|h| h.cpu_cores).unwrap_or(0),
        // uptime 由 boot_at 反推，所以 agent 重启不会让在线时长归零
        uptime_s: h.map(|h| (now - h.boot_at).max(0)).unwrap_or(0),
        last_seen: e.last_seen,
        agent_version: h.map(|h| h.agent_version.clone()).unwrap_or_default(),

        cpu_pct: e
            .ring
            .back()
            .and_then(|r| r.cpu_pct)
            .map(|bp| bp as f32 / 100.0)
            .unwrap_or(0.0),
        // 平台没有负载概念时给 None 而不是 0.0 —— 前端会整行隐藏
        load1: (caps.load_average)
            .then(|| m.map(|m| f32::from(m.load[0]) / 100.0))
            .flatten(),
        load5: (caps.load_average)
            .then(|| m.map(|m| f32::from(m.load[1]) / 100.0))
            .flatten(),
        load15: (caps.load_average)
            .then(|| m.map(|m| f32::from(m.load[2]) / 100.0))
            .flatten(),
        mem,
        disk,
        net: m
            .map(|m| NetView {
                rx_bytes: m.net.rx_bytes,
                tx_bytes: m.net.tx_bytes,
                rx_speed: m.net.rx_speed,
                tx_speed: m.net.tx_speed,
                ifaces: m.net.ifaces.clone(),
            })
            .unwrap_or_default(),
        tcp_conn: m.and_then(|m| m.tcp_conn),
        proc_count: m.and_then(|m| m.proc_count),
        cpu_temp: m.and_then(|m| m.cpu_temp).map(|t| t as f32 / 10.0),
        latency: e.latency,
        latency_spark: e.spark.iter().map(|(r, _)| *r).collect(),
        loss_spark: e.spark.iter().map(|(_, l)| *l).collect(),
        gpu: m.and_then(|m| m.gpu).map(|g| GpuView {
            util: f32::from(g.util) / 100.0,
            mem_used: g.mem_used,
            mem_total: g.mem_total,
            temp: g.temp as f32 / 10.0,
        }),
        capabilities: caps,
    }
}

/// 百分比，分母为 0 时返回 0 而不是 NaN —— NaN 序列化成 JSON 会变成 null，
/// 前端的 `.toFixed` 会直接崩。
fn pct(used: u64, total: u64) -> f32 {
    if total == 0 {
        return 0.0;
    }
    (used as f64 * 100.0 / total as f64) as f32
}

pub fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 由 token 派生一个稳定的公开标识。
///
/// M0/M1 用非加密哈希即可 —— 它只是个 map key，从不作为凭据、也从不用于校验。
/// M3 会换成数据库里的 `SHA-256(token)` 比对。
pub fn derive_id(token: &str) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    token.hash(&mut h);
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> Hello {
        Hello {
            proto_version: 1,
            agent_version: "0.0.1".into(),
            hostname: "test-host".into(),
            os: "Linux".into(),
            arch: "x86_64".into(),
            cpu_cores: 2,
            boot_at: now_unix() - 3600,
            kernel: None,
            cpu_model: None,
            virtualization: None,
            mem_total: 0,
            swap_total: 0,
            disk_total: 0,
            interfaces: Vec::new(),
            capabilities: Default::default(),
        }
    }

    fn metrics(ts: i64, cpu_pct: u16) -> Metrics {
        Metrics {
            ts,
            cpu_pct,
            ..Default::default()
        }
    }

    #[test]
    fn lifecycle_online_then_offline() {
        let st = AppState::new();
        let id = derive_id("tok");
        assert_eq!(st.summary().total, 0);

        st.on_connect(&id, mpsc::unbounded_channel().0);
        st.on_hello(&id, hello());
        st.on_metrics(&id, metrics(now_unix(), 1234));

        let s = st.summary();
        assert_eq!((s.total, s.online, s.offline), (1, 1, 0));
        let list = st.list();
        assert_eq!(list[0].name, "test-host");
        assert!((list[0].cpu_pct - 12.34).abs() < 0.01);
        assert!(list[0].uptime_s >= 3600, "uptime 应由 boot_at 反推");

        st.on_disconnect(&id);
        assert_eq!(st.summary().online, 0, "断开必须立刻离线，不等超时");
    }

    #[test]
    fn metrics_before_hello_does_not_panic() {
        let st = AppState::new();
        let id = derive_id("tok");
        st.on_metrics(&id, metrics(now_unix(), 5000));
        let list = st.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "(未上报)");
    }

    #[test]
    fn stale_metrics_count_as_offline_even_if_connected() {
        // agent 进程卡死：WS 还连着，但不再上报
        let st = AppState::new();
        let id = derive_id("tok");
        st.on_connect(&id, mpsc::unbounded_channel().0);
        st.inner.get_mut(&id).unwrap().last_seen = now_unix() - (OFFLINE_GRACE_S + 1);
        assert_eq!(st.summary().online, 0);
    }

    #[test]
    fn ring_is_bounded() {
        let st = AppState::new();
        let id = derive_id("tok");
        for i in 0..(RING_CAP as i64 * 3) {
            st.on_metrics(&id, metrics(i, 1));
        }
        let e = st.inner.get(&id).unwrap();
        assert_eq!(e.ring.len(), RING_CAP, "环形缓冲必须有界，否则内存无限增长");
        assert_eq!(
            e.ring.front().unwrap().ts,
            RING_CAP as i64 * 2,
            "应淘汰最旧的"
        );
    }

    #[test]
    fn 没上报过的机器也能查到主键() {
        // `set_db_id` 用 entry 建条目，所以启动时从库里回填是够的 ——
        // 不需要等 agent 连上来。
        //
        // 这条锁的是详情页那个 404：映射原本**只在 agent 连上的那一刻**建立，
        // 于是面板一重启，所有机器的详情页都 404 到探针重连为止；
        // 从没装过探针的机器则永远打不开。真机上点开卡片才发现的。
        let st = AppState::new();
        assert_eq!(st.db_id("never-seen"), None);
        st.set_db_id("never-seen", 7);
        assert_eq!(st.db_id("never-seen"), Some(7));
    }

    #[test]
    fn 删掉的机器不会赖在内存里() {
        let st = AppState::new();
        st.set_db_id("gone", 9);
        assert_eq!(st.db_id("gone"), Some(9));
        st.forget("gone");
        assert_eq!(st.db_id("gone"), None, "不清的话它会一直显示成一台离线机器");
    }

    #[test]
    fn snapshot_skips_servers_without_db_id() {
        // 还没 upsert 进数据库的机器不能落盘，否则 FK 会指向不存在的行
        let st = AppState::new();
        let id = derive_id("tok");
        st.on_metrics(&id, metrics(60, 100));
        assert!(st.snapshot_minute(60).is_empty());

        st.set_db_id(&id, 42);
        st.on_metrics(&id, metrics(61, 300));
        let snap = st.snapshot_minute(60);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].server_id, 42);
        assert_eq!(snap[0].ts, 60, "落盘时间戳必须对齐到分钟起点");
    }

    #[test]
    fn snapshot_only_covers_its_own_minute() {
        let st = AppState::new();
        let id = derive_id("tok");
        st.set_db_id(&id, 1);
        st.on_metrics(&id, metrics(59, 999)); // 上一分钟
        st.on_metrics(&id, metrics(60, 100));
        st.on_metrics(&id, metrics(119, 300));
        st.on_metrics(&id, metrics(120, 999)); // 下一分钟

        let snap = st.snapshot_minute(60);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].cpu_pct, Some(200), "只能聚合本分钟内的两个点");
    }

    #[test]
    fn snapshot_of_idle_minute_is_empty() {
        let st = AppState::new();
        let id = derive_id("tok");
        st.set_db_id(&id, 1);
        st.on_metrics(&id, metrics(60, 100));
        // 机器离线的那一分钟不该写入一行全 NULL 的占位数据
        assert!(st.snapshot_minute(120).is_empty());
    }

    #[test]
    fn derive_id_is_deterministic_and_hides_token() {
        let a = derive_id("super-secret-token");
        assert_eq!(a, derive_id("super-secret-token"));
        assert_ne!(a, derive_id("other-token"));
        assert!(!a.contains("secret"));
    }
}
