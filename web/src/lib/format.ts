/**
 * 展示层的纯函数。全部可单测 —— 这是前端里唯一有逻辑的部分，
 * 也是最容易出细节错误的地方（单位、♾️、时区、国旗）。
 */

const KIB = 1024

/** 字节数 → 人类可读。分母用 1024，与 `df -h` 一致。 */
export function bytes(n: number | null | undefined, digits = 1): string {
  if (n === null || n === undefined || !Number.isFinite(n)) return '—'
  if (n < 0) return '—'
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB']
  let v = n
  let i = 0
  while (v >= KIB && i < units.length - 1) {
    v /= KIB
    i++
  }
  // 字节数本身不带小数
  return `${i === 0 ? v : v.toFixed(digits)} ${units[i]}`
}

/** 速率。B/s → 可读，带 /s 后缀。 */
export function speed(n: number | null | undefined): string {
  const s = bytes(n, 1)
  return s === '—' ? s : `${s}/s`
}

/**
 * 流量上限。**未配置时显示 ♾️ 而不是 0**（R16）。
 */
export function limit(n: number | null | undefined): string {
  return n === null || n === undefined || n <= 0 ? '♾️' : bytes(n)
}

/** 秒 → "3天5时" / "5时12分" / "12分"。 */
export function duration(sec: number | null | undefined): string {
  if (sec === null || sec === undefined || !Number.isFinite(sec) || sec < 0) return '—'
  const d = Math.floor(sec / 86400)
  const h = Math.floor((sec % 86400) / 3600)
  const m = Math.floor((sec % 3600) / 60)
  if (d > 0) return `${d}天${h}时`
  if (h > 0) return `${h}时${m}分`
  return `${m}分`
}

/**
 * 剩余天数。
 *
 * - `null` + `infinite` → ♾️（买断机器）
 * - `null` 且非买断 → —（未设到期时间，**不能显示 0 天**）
 * - 负数 → 「已过期 N 天」
 */
export function remainDays(
  days: number | null | undefined,
  infinite = false,
): string {
  if (infinite) return '♾️'
  if (days === null || days === undefined) return '—'
  if (days < 0) return `已过期 ${-days} 天`
  return `${days} 天`
}

/** 节点价格。
 *
 * 价格语义是用户定的约定，三个分支缺一不可：
 *   **-1 = 免费**（白嫖来的机器，要专门标出来）
 *   **0  = 不显示**（还没填，或者不想公开）
 *   \>0 = 正常显示金额
 *
 * 后端 `price_display` 是照实换算的原始值 —— 免费机器那里会是个负数
 * （-1 × 汇率 = -6.71）。**特判在展示层，所以这个函数是唯一入口**：
 * 卡片、列表、详情页曾经各抄了一份一模一样的判断，
 * 而「免费机器剩余价值显示成 ¥-6.71」正是同类漏改造成的。
 */
export function priceKind(
  b: { price: number } | null | undefined,
): 'hidden' | 'free' | 'amount' {
  if (!b || b.price === 0) return 'hidden'
  return b.price < 0 ? 'free' : 'amount'
}

/** 价格的文本形式。卡片要的是两种不同底色的 Badge，用上面的 `priceKind` 自己渲染。 */
export function price(
  b: { price: number; price_display: number | null } | null | undefined,
  currency: string,
): string {
  switch (priceKind(b)) {
    case 'hidden':
      return '—'
    case 'free':
      return '免费'
    case 'amount':
      return money(b!.price_display, currency)
  }
}

/** 百分比。分母未知（total=0）时后端给 0，这里按调用方传入的可用性判断。 */
export function pct(v: number | null | undefined, digits = 1): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return '—'
  return `${v.toFixed(digits)}%`
}

/** 金额。`null` 表示该货币没有汇率 —— 显示 — 而不是 0。 */
export function money(v: number | null | undefined, currency: string): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return '—'
  const symbol: Record<string, string> = { CNY: '¥', USD: '$', EUR: '€', GBP: '£', JPY: '¥' }
  const digits = ['JPY', 'KRW', 'VND'].includes(currency) ? 0 : 2
  return `${symbol[currency] ?? ''}${v.toFixed(digits)}${symbol[currency] ? '' : ' ' + currency}`
}

/**
 * ISO 3166-1 alpha-2 → 国旗 emoji。
 *
 * 用 Regional Indicator 码点合成，零依赖、无版权问题。
 */
export function countryToFlag(code: string | null | undefined): string | null {
  if (!code || code.length !== 2 || !/^[A-Za-z]{2}$/.test(code)) return null
  const A = 0x1f1e6
  return String.fromCodePoint(
    ...[...code.toUpperCase()].map((c) => A + c.charCodeAt(0) - 65),
  )
}

/**
 * 本环境能否渲染国旗 emoji。
 *
 * ⚠️ **Windows 没有国旗 emoji 字体**，`🇺🇸` 会被渲染成 "US" 两个字母。
 * 一半用户看到的是字母而不是国旗，所以必须检测并回落。
 *
 * 方法：量一次 `🇺🇸` 的宽度。若与两个字母的宽度相当，说明没有合成国旗。
 * 拿不到 canvas（SSR / 隐私模式）时保守地当作**支持** ——
 * 显示 emoji 失败只是难看，回落到纯文字则是必然更差。
 */

/** unix 秒 → 本地可读时间。 */
export function time(ts: number | null | undefined): string {
  if (!ts) return '—'
  const d = new Date(ts * 1000)
  const p = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
}

/** 进度条的颜色分档。80% 起变黄，95% 起变红。 */
export function level(p: number | null | undefined): 'ok' | 'warn' | 'danger' {
  if (p === null || p === undefined || !Number.isFinite(p)) return 'ok'
  if (p >= 95) return 'danger'
  if (p >= 80) return 'warn'
  return 'ok'
}
