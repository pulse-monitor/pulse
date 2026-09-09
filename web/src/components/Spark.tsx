/**
 * 迷你走势图。**纯 SVG 手绘，不引图表库** ——
 * 卡片上这条 100×16 的线不值得为它把 uPlot 拉进首屏包（那是 23 KB gzip）。
 *
 * 空数组时画一条占位的灰线，而不是什么都不画：
 * 卡片高度保持一致，不会因为某台机器没配延迟监控就整排错位。
 */
export function Spark({ values, tone }: { values: number[]; tone: 'sky' | 'amber' }) {
  const W = 100
  const H = 16
  const stroke = tone === 'sky' ? 'rgb(14 165 233)' : 'rgb(245 158 11)'

  if (values.length < 2) {
    return (
      <svg viewBox={`0 0 ${W} ${H}`} className="h-4 w-full" aria-hidden>
        <line x1="0" y1={H - 1} x2={W} y2={H - 1} stroke="currentColor" strokeOpacity="0.15" />
      </svg>
    )
  }

  const max = Math.max(...values)
  const min = Math.min(...values)
  // 全平（比如丢包恒为 0）时不能除以 0，画在底部
  const span = max - min || 1
  const step = W / (values.length - 1)
  const pts = values.map((v, i) => {
    const x = i * step
    const y = H - 1 - ((v - min) / span) * (H - 2)
    return `${x.toFixed(1)},${y.toFixed(1)}`
  })

  return (
    <svg
      viewBox={`0 0 ${W} ${H}`}
      className="h-4 w-full"
      preserveAspectRatio="none"
      role="img"
      aria-label={`最近 ${values.length} 次，最低 ${min.toFixed(1)}，最高 ${max.toFixed(1)}`}
    >
      <polyline
        points={pts.join(' ')}
        fill="none"
        stroke={stroke}
        strokeWidth="1.5"
        strokeLinejoin="round"
        vectorEffect="non-scaling-stroke"
      />
    </svg>
  )
}
