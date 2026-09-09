/**
 * 一套 16×16 的线性小图标。
 *
 * 自己画而不是引图标库：全站一共用到七八个，为它加一个几十 KB 的依赖不划算，
 * 而且这样能保证描边粗细、圆角、留白在同一套规格上（引库常常混着两三种风格）。
 *
 * 都用 `currentColor`，颜色交给调用方的 `className` 决定 ——
 * 统计卡里是统一的灰，服务器卡片里按指标分色（CPU 蓝、内存绿、硬盘橙、流量紫），
 * 分色是为了扫一眼就能定位到某一项，不是为了好看。
 */
type P = { className?: string }

const base = 'h-4 w-4 shrink-0'
const S = ({ className = '', children }: P & { children: React.ReactNode }) => (
  <svg
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.6"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
    className={`${base} ${className}`}
  >
    {children}
  </svg>
)

/** 处理器：一块带引脚的芯片 */
export const IconCpu = ({ className }: P) => (
  <S className={className}>
    <rect x="7" y="7" width="10" height="10" rx="1.6" />
    <path d="M10 3v4M14 3v4M10 17v4M14 17v4M3 10h4M3 14h4M17 10h4M17 14h4" />
  </S>
)

/** 内存：内存条 */
export const IconMemory = ({ className }: P) => (
  <S className={className}>
    <rect x="3" y="7" width="18" height="10" rx="2" />
    <path d="M7 7V4M12 7V4M17 7V4M7 20v-3M12 20v-3M17 20v-3" />
  </S>
)

/** 硬盘：两层盘片 */
export const IconDisk = ({ className }: P) => (
  <S className={className}>
    <rect x="3" y="5" width="18" height="6" rx="2" />
    <rect x="3" y="13" width="18" height="6" rx="2" />
    <path d="M7 8h.01M7 16h.01" />
  </S>
)

/** 流量：上下两个箭头 */
export const IconTraffic = ({ className }: P) => (
  <S className={className}>
    <path d="M8 20V7M8 7L5 10M8 7l3 3" />
    <path d="M16 4v13M16 17l3-3M16 17l-3-3" />
  </S>
)

/** 上行 */
export const IconUp = ({ className }: P) => (
  <S className={className}>
    <path d="M6 13l6-6 6 6M6 19l6-6 6 6" />
  </S>
)

/** 下行 */
export const IconDown = ({ className }: P) => (
  <S className={className}>
    <path d="M6 11l6 6 6-6M6 5l6 6 6-6" />
  </S>
)

/** 累计用量：一段走势 */
export const IconChart = ({ className }: P) => (
  <S className={className}>
    <path d="M4 16h4l3-9 3 13 3-8h3" />
  </S>
)

/** 钱包 */
export const IconWallet = ({ className }: P) => (
  <S className={className}>
    <rect x="3" y="6" width="18" height="13" rx="2.5" />
    <path d="M3 10h18" />
    <circle cx="17" cy="14.5" r="1.1" fill="currentColor" stroke="none" />
  </S>
)

/** 延迟：一个表盘 */
export const IconLatency = ({ className }: P) => (
  <S className={className}>
    <circle cx="12" cy="12" r="8.5" />
    <path d="M12 7.5V12l3 2" />
  </S>
)

/** 丢包：断开的连线 */
export const IconLoss = ({ className }: P) => (
  <S className={className}>
    <path d="M4 12h4M16 12h4" />
    <path d="M10.5 9.5l3 5M13.5 9.5l-3 5" />
  </S>
)
