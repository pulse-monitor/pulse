/**
 * Pulse 的标识：一条心电式的脉搏波，末端一个跳动的圆点。
 *
 * 为什么是这个：项目做的是「这台机器还活着吗」——心跳是最直白的隐喻，
 * 而且波形本身就是监控图表的形状。
 *
 * 纯 SVG、单色（`currentColor`），跟着文字颜色和主题走，
 * 不需要为暗色单独准备一份。整个图标不到 400 字节。
 */
export function Logo({ className = '' }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 32 32"
      className={`inline-block h-[1.1em] w-[1.1em] shrink-0 ${className}`}
      fill="none"
      role="img"
      aria-label="Pulse"
    >
      {/* 脉搏波。stroke 用 currentColor，颜色由外部决定 */}
      <path
        d="M2 18h5.5l3-9 4.5 18 3.5-11 2.5 6H27"
        stroke="currentColor"
        strokeWidth="2.6"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      {/* 末端的点：波形走到这儿「还在跳」 */}
      <circle cx="28.5" cy="18" r="2.6" fill="currentColor" />
    </svg>
  )
}
