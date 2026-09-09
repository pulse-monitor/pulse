/** 与面板的所有通信。类型与 的契约一一对应。 */

/** 生产是同源；开发时 vite 代理 /api。也允许运行时注入（前后端分离部署）。 */
export const API_BASE: string =
  (globalThis as { PULSE_API_BASE?: string }).PULSE_API_BASE ?? ''

/** access token 只放内存 —— 放 localStorage 会被 XSS 直接偷走。 */
let accessToken: string | null = null
export const setToken = (t: string | null) => {
  accessToken = t
}
export const hasToken = () => accessToken !== null

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message)
  }
}

async function req<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers)
  if (init.body) headers.set('Content-Type', 'application/json')
  if (accessToken) headers.set('Authorization', `Bearer ${accessToken}`)

  const r = await fetch(`${API_BASE}${path}`, {
    ...init,
    headers,
    credentials: 'include', // refresh cookie
  })
  if (r.status === 204) return undefined as T
  const text = await r.text()
  if (!r.ok) {
    // 后端的错误体是 {error:{code,message}}；解析不出来就用原文
    let code = 'HTTP_ERROR'
    let message = text || r.statusText
    try {
      const j = JSON.parse(text)
      if (j?.error) {
        code = j.error.code
        message = j.error.message
      }
    } catch {
      /* 用原文 */
    }
    throw new ApiError(r.status, code, message)
  }
  return text ? (JSON.parse(text) as T) : (undefined as T)
}

export const get = <T,>(p: string) => req<T>(p)
export const post = <T,>(p: string, body?: unknown) =>
  req<T>(p, { method: 'POST', body: body === undefined ? undefined : JSON.stringify(body) })
export const put = <T,>(p: string, body?: unknown) =>
  req<T>(p, { method: 'PUT', body: body === undefined ? undefined : JSON.stringify(body) })
export const del = <T,>(p: string) => req<T>(p, { method: 'DELETE' })

// ---------------------------------------------------------------------------
// 类型
// ---------------------------------------------------------------------------

export interface Capabilities {
  icmp_unprivileged: boolean
  gpu_nvml: boolean
  /** false 时前端**隐藏**温度行，而不是显示 0 */
  temperature: boolean
  tcp_conn_count: boolean
  /** hidepid 下为 false —— 读到的是错数，不能显示 */
  proc_count: boolean
  /** Windows 没有负载概念 */
  load_average: boolean
  self_update: boolean
  /** 容器且未挂 lxcfs：规格来自 cgroup，UI 要提示 */
  cgroup_limited: boolean
}

export interface Server {
  id: string
  name: string
  online: boolean
  os: string
  kernel: string | null
  arch: string
  virtualization: string | null
  country_code: string | null
  latitude: number | null
  longitude: number | null
  /** 所属分组（R11）。null = 未分组 */
  group_id: number | null
  /** 购买同款 / 测评链接（R16）。为空时卡片上不显示那个按钮 */
  buy_url: string | null
  review_url: string | null
  cpu_model: string | null
  cpu_cores: number
  uptime_s: number
  last_seen: number
  agent_version: string
  cpu_pct: number
  load1: number | null
  load5: number | null
  load15: number | null
  mem: { total: number; free: number; available: number; used: number; used_with_cache: number; pct: number; swap_total: number; swap_used: number }
  disk: { total: number; used: number; pct: number }
  net: { rx_bytes: number; tx_bytes: number; rx_speed: number; tx_speed: number; ifaces: string[] }
  tcp_conn: number | null
  proc_count: number | null
  cpu_temp: number | null
  gpu: { util: number; mem_used: number; mem_total: number; temp: number } | null
  latency: { task_id: number; rtt_ms: number; loss_pct: number } | null
  /** 最近若干次延迟 / 丢包，卡片上的迷你走势图用。空数组 = 还没探测过 */
  latency_spark: number[]
  loss_spark: number[]
  capabilities: Capabilities
}

export interface Billing {
  price: number
  currency: string
  cycle: string
  expire_at: number | null
  auto_renew: boolean
  remain_days: number | null
  remaining_value: number
  infinite: boolean
  unknown_expiry: boolean
  price_display: number | null
  remaining_display: number | null
  rate_level: 'fresh' | 'stale' | 'manual' | 'missing'
  renew_state: 'ok' | 'renewing' | 'expired'
}

export interface Traffic {
  in_bytes: number
  out_bytes: number
  used: number
  /** null = 无限，前端显示 ♾️ */
  limit: number | null
  pct: number | null
  calc_mode: string
  period_start: number
  reset_day: number
}

export interface ServerEntry {
  server: Server
  billing: Billing | null
  traffic: Traffic | null
}

export interface Summary {
  servers: { total: number; online: number; offline: number }
  network: { in_speed: number; out_speed: number }
  value: {
    display_currency: string
    total_value: number
    annual_cost: number
    total_remaining: number
    /** 三个诚实性字段：汇总不完整时前端必须显示出来 */
    unpriced_servers: number
    no_expire_servers: number
    no_rate_servers: number
  }
  rate: { as_of: string; fetched_at: number; stale: boolean }
  updated_at: number
}

export interface Group {
  id: number
  name: string
  color: string | null
  icon: string | null
  sort_order: number
}

export interface ExpiringItem {
  id: string
  name: string
  country_code: string | null
  remain_days: number
  expire_at: number | null
  auto_renew: boolean
  renew_state: 'ok' | 'renewing' | 'expired'
  remaining_display: number | null
}

export interface Series {
  granularity: string | null
  step: number
  ts: number[]
  [k: string]: unknown
}

export const api = {
  servers: () => get<{ servers: ServerEntry[]; display_currency: string }>('/api/v1/public/servers'),
  summary: () => get<Summary>('/api/v1/public/summary'),
  groups: () => get<Group[]>('/api/v1/public/groups'),
  expiring: (days = 7) =>
    get<{ days: number; display_currency: string; items: ExpiringItem[] }>(
      `/api/v1/public/expiring?days=${days}`,
    ),
  metrics: (uuid: string, range: string) =>
    get<Series>(`/api/v1/public/servers/${uuid}/metrics?range=${range}`),
  ping: (uuid: string, range: string, taskId = 1) =>
    get<Series>(`/api/v1/public/servers/${uuid}/ping?range=${range}&task_id=${taskId}`),
  /** 这台机器实际生效的延迟任务。只有 id 和名称 —— host / 间隔属于运维配置 */
  pingTasks: (uuid: string) =>
    get<{ id: number; name: string }[]>(`/api/v1/public/servers/${uuid}/ping-tasks`),
  visitor: () =>
    get<{
      ip: string
      os: string | null
      browser: string | null
      time: number
      timezone: string
      /** IDC / 运营商。GeoIP 自动解析尚未启用，目前恒为 null */
      isp: string | null
      asn: number | null
    }>('/api/v1/public/visitor'),
  login: (username: string, password: string) =>
    post<{ access_token: string; expires_in: number }>('/api/v1/auth/login', { username, password }),
  refresh: () => post<{ access_token: string; expires_in: number }>('/api/v1/auth/refresh'),
  logout: () => post<void>('/api/v1/auth/logout'),
}
