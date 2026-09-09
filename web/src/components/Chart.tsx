import { useEffect, useRef } from 'react'
import 'uplot/dist/uPlot.min.css'

/** uPlot 实例上我们用到的那一小部分。不引它的类型包，就地声明。 */
export interface UPlot {
  data: (number | null)[][]
}

/** 最后一个**有值**的点的下标。末尾常有几个 null（那一段还没上报） */
export function lastIdx(u: UPlot): number {
  const xs = u.data[0] ?? []
  for (let i = xs.length - 1; i >= 0; i--) {
    if (u.data.some((col, c) => c > 0 && col?.[i] != null)) return i
  }
  return xs.length - 1
}

/**
 * 没给 `fmt` 时的默认数字格式。
 *
 * 直接 `String(v)` 会把 `646.0238285064697` 原样丢进图例 —— 十几位小数
 * 没有任何意义，还会把图例撑得老长。按量级给位数：大数不要小数，小数多给两位。
 */
export function fmtNum(v: number | null): string {
  if (v == null || !Number.isFinite(v)) return '—'
  const a = Math.abs(v)
  return v.toFixed(a >= 100 ? 0 : a >= 10 ? 1 : 2)
}

/** 图例里的时间。x 轴是秒级时间戳 */
export function fmtTime(sec: number | null): string {
  if (sec == null) return '--'
  const d = new Date(sec * 1000)
  const p = (n: number) => String(n).padStart(2, '0')
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
}

export interface Line {
  label: string
  data: (number | null)[]
  /** CSS 颜色 */
  color: string
  /** 值 → 显示文本 */
  fmt?: (v: number | null) => string
}

/**
 * 折线图。
 *
 * 用 uPlot 而不是 ECharts：单页可能同时有多张图 + 200 个迷你走势，
 * uPlot 约 45 KB、单图约 1 ms；ECharts 完整包 1 MB+，200 个实例直接卡死。
 * 代价是要自己写一层封装（就是这个文件）。
 */
export function Chart({
  ts,
  lines,
  height = 180,
  title,
}: {
  ts: number[]
  lines: Line[]
  height?: number
  title?: string
}) {
  const box = useRef<HTMLDivElement>(null)
  const plot = useRef<{ destroy(): void; setSize(s: { width: number; height: number }): void } | null>(null)

  useEffect(() => {
    const el = box.current
    if (!el || ts.length === 0) return
    let cancelled = false

    // 动态 import：图表只在详情页用，不该进首屏包
    import('uplot').then(({ default: uPlot }) => {
      if (cancelled || !box.current) return
      const dark =
        document.documentElement.dataset.theme === 'dark' ||
        (!document.documentElement.dataset.theme &&
          window.matchMedia('(prefers-color-scheme: dark)').matches)
      const axis = dark ? '#71717a' : '#a1a1aa'
      const grid = dark ? 'rgba(255,255,255,.07)' : 'rgba(0,0,0,.07)'

      const opts = {
        width: box.current.clientWidth || 600,
        height,
        title,
        cursor: { sync: { key: 'pulse' } },
        legend: { live: true },
        scales: { x: { time: true } },
        axes: [
          { stroke: axis, grid: { stroke: grid }, ticks: { stroke: grid } },
          { stroke: axis, grid: { stroke: grid }, ticks: { stroke: grid } },
        ],
        series: [
          {
            label: '时间',
            // 鼠标不在图上时，uPlot 给 idx = null，图例就成了一排「--」。
            // 一张图默认什么都不说是不对的 —— 退回显示**最后一个点**，
            // 这也正是用户最想知道的「现在是多少」。
            value: (u: UPlot, _v: number | null, _si: number, i: number | null) =>
              fmtTime(u.data[0]?.[i ?? lastIdx(u)] ?? null),
          },
          ...lines.map((l, li) => ({
            label: l.label,
            stroke: l.color,
            width: 1.5,
            value: (u: UPlot, v: number | null, _si: number, i: number | null) => {
              const val = i == null ? (u.data[li + 1]?.[lastIdx(u)] ?? null) : v
              return l.fmt ? l.fmt(val) : fmtNum(val)
            },
          })),
        ],
      }
      const data = [ts, ...lines.map((l) => l.data)]
      plot.current?.destroy()
      // @ts-expect-error uPlot 的类型对 data 要求过严，运行时接受这个形状
      plot.current = new uPlot(opts, data, box.current)
    })

    const ro = new ResizeObserver(() => {
      if (box.current && plot.current) {
        plot.current.setSize({ width: box.current.clientWidth, height })
      }
    })
    ro.observe(el)
    return () => {
      cancelled = true
      ro.disconnect()
      plot.current?.destroy()
      plot.current = null
    }
  }, [JSON.stringify(ts), JSON.stringify(lines.map((l) => l.data)), height, title])

  if (ts.length === 0) {
    return (
      <div
        className="flex items-center justify-center rounded-lg border border-dashed border-black/10 text-sm text-zinc-500 dark:border-white/10"
        style={{ height }}
      >
        这个时间范围内没有数据
      </div>
    )
  }
  return <div ref={box} className="w-full" />
}
