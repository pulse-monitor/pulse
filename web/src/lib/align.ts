/**
 * 把多条各自独立的时间序列对齐到同一条时间轴上。
 *
 * uPlot 要求所有折线共用一个 x 数组，而每个延迟探测点是**分别查出来的**。
 * 好在它们用同一个分桶步长（由时间范围决定，不是由任务决定），
 * 所以时间戳天然落在同一个网格上 —— 这里是精确对齐，不是就近取值。
 *
 * 某个探测点在某一刻没有数据（任务刚建、或者那段时间探针离线）就填 `null`，
 * uPlot 会把线断开。**不能填 0** —— 0 毫秒延迟是个假消息。
 */
export function alignSeries(
  series: { ts: number[]; values: (number | null)[] }[],
): { ts: number[]; cols: (number | null)[][] } {
  const all = new Set<number>()
  for (const s of series) for (const t of s.ts) all.add(t)
  const ts = [...all].sort((a, b) => a - b)
  const at = new Map(ts.map((t, i) => [t, i]))

  const cols = series.map((s) => {
    const col: (number | null)[] = new Array(ts.length).fill(null)
    for (let i = 0; i < s.ts.length; i++) {
      const j = at.get(s.ts[i]!)
      if (j !== undefined) col[j] = s.values[i] ?? null
    }
    return col
  })
  return { ts, cols }
}

/**
 * 给 N 条线分配颜色。
 *
 * 固定表而不是按 hash 生成：hash 出来的颜色明暗不一，有的在暗色主题里
 * 根本看不见。超过表长就循环 —— 十个探测点画在一张图上本来就读不清了，
 * 与其保证颜色唯一，不如保证每个颜色都看得见。
 */
export const LINE_COLORS = [
  '#0ea5e9', // sky
  '#10b981', // emerald
  '#f59e0b', // amber
  '#a855f7', // violet
  '#ef4444', // red
  '#14b8a6', // teal
  '#ec4899', // pink
  '#84cc16', // lime
] as const

export function lineColor(i: number): string {
  return LINE_COLORS[i % LINE_COLORS.length]!
}
