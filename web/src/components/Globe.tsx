import { useEffect, useMemo, useRef, useState } from 'react'
import type { ServerEntry } from '../api'
import { basis, countryPath, prepare, project, projectLonLat, type Country, type LonLat, type View } from '../lib/globe'
import { COUNTRY_LATLON } from '../lib/countries'
import { Flag } from './Flag'

/**
 * 服务器分布地球（R10）。
 *
 * 两个要求要同时满足：**是个球**，而且**有机器的国家整块点亮**。
 * 早先用过 cobe —— 它画的是点阵球，填不了国家多边形；后来换成平面 SVG 世界地图 ——
 * 能点亮国家但不是球了。现在是把国家的经纬度多边形自己做正交投影画到球面上，
 * 投影和地平线裁剪都在 `lib/globe.ts` 里，有 20 个单测盯着。
 *
 * 数据 `public/world-geo.json`（构建期由 `scripts/gen-globe.mjs` 生成，
 * 174 个国家、gzip 37 KB）**按需加载**：不看分布就不下载。
 *
 * 国家归属来自 GeoIP（`crates/pulse-server/src/domain/geoip.rs`），不用手填。
 */

const R = 150 // 球半径（viewBox 单位）
const PAD = 6 // 留点边，描边不会被裁掉
const SIZE = (R + PAD) * 2

/** 每秒转多少度。转一圈约 72 秒 —— 慢到不打扰阅读，又看得出在动。 */
const SPIN = 5

interface CountryStat {
  total: number
  online: number
}

export function Globe({
  servers,
  className = '',
}: {
  servers: ServerEntry[]
  className?: string
}) {
  const [geo, setGeo] = useState<Record<string, Country> | null>(null)
  const [failed, setFailed] = useState(false)
  const [hover, setHover] = useState<string | null>(null)
  const [lambda, setLambda] = useState(-30) // 起手让大西洋两岸都能看见
  const [phi, setPhi] = useState(18) // 略微俯视，北半球陆地多
  const spinning = useRef(true)
  const drag = useRef<{ x: number; y: number } | null>(null)

  useEffect(() => {
    let alive = true
    fetch('/world-geo.json')
      .then((r) => (r.ok ? r.json() : Promise.reject(new Error(String(r.status)))))
      .then((d: Record<string, LonLat[][]>) => alive && setGeo(prepare(d)))
      // 地图取不到不影响别的：下面的国家列表照常显示
      .catch(() => alive && setFailed(true))
    return () => {
      alive = false
    }
  }, [])

  // 自转。用时间差而不是固定步长 —— 掉帧时转速才不会跟着变慢。
  useEffect(() => {
    // 用户在系统里关了动效就别转，这是无障碍的基本要求
    if (window.matchMedia?.('(prefers-reduced-motion: reduce)').matches) return
    let raf = 0
    let last = performance.now()
    const tick = (t: number) => {
      const dt = Math.min(t - last, 100) / 1000 // 切回标签页时别一次转过去
      last = t
      if (spinning.current && !drag.current) setLambda((l) => (l + SPIN * dt) % 360)
      raf = requestAnimationFrame(tick)
    }
    raf = requestAnimationFrame(tick)
    return () => cancelAnimationFrame(raf)
  }, [])

  const byCountry = useMemo(() => {
    const m = new Map<string, CountryStat>()
    for (const e of servers) {
      const c = e.server.country_code?.toUpperCase()
      if (!c) continue
      const cur = m.get(c) ?? { total: 0, online: 0 }
      cur.total++
      if (e.server.online) cur.online++
      m.set(c, cur)
    }
    return m
  }, [servers])

  const legend = useMemo(
    () => [...byCountry.entries()].sort((a, b) => b[1].total - a[1].total),
    [byCountry],
  )

  const view: View = { cx: R + PAD, cy: R + PAD, r: R, lambda0: lambda, phi0: phi }
  const b = basis(view)

  /**
   * 小到画不出来的国家，用标记点代替。
   *
   * 新加坡、香港这类在 110m 地图里只有几个点，投到三百像素的球上不到一个像素，
   * 填色等于没填。可换 50m 数据也没用 —— 体积翻五倍（gzip 37 KB → 194 KB），
   * 而它们照样是亚像素。所以小国一律画成一个看得见的点。
   */
  const MIN_PX = 5

  // 先画没机器的，再画点亮的 —— 后画的盖在上面，边界不会被邻国压住
  const { paths, dots } = useMemo(() => {
    const plain: [string, string][] = []
    const lit: [string, string][] = []
    const dots: [string, number, number][] = []
    const drawn = new Set<string>()

    if (geo) {
      for (const [cc, country] of Object.entries(geo)) {
        const litOne = byCountry.has(cc)
        // 角半径 × 球半径 = 屏幕上的大致尺寸
        if (litOne && country.extent * R * 2 < MIN_PX) {
          const p = project(country.center, view, b)
          if (p.depth > 0) dots.push([cc, p.x, p.y])
          drawn.add(cc)
          continue
        }
        const d = countryPath(country.rings, view, b)
        if (!d) {
          if (litOne) drawn.add(cc) // 转到背面了，别再当成「地图里没有」
          continue
        }
        ;(litOne ? lit : plain).push([cc, d])
        drawn.add(cc)
      }
    }

    // 地图数据里压根没有的国家（110m 收录 174 个，有些微型国家不在其中），
    // 退回用经纬度表打点 —— 总比在地图上完全看不到强
    for (const cc of byCountry.keys()) {
      if (drawn.has(cc)) continue
      const ll = COUNTRY_LATLON[cc]
      if (!ll) continue
      const p = projectLonLat([ll[1], ll[0]], view, b)
      if (p.depth > 0) dots.push([cc, p.x, p.y])
    }

    return { paths: [...plain, ...lit], dots }
    // view / b 由 lambda、phi 决定，列进依赖会因为对象每次新建而失效
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [geo, lambda, phi, byCountry])

  if (legend.length === 0) return null

  const hoverStat = hover ? byCountry.get(hover) : null

  /** 拖动转球。按住时暂停自转，松手恢复。 */
  const onDown = (ev: React.PointerEvent<SVGSVGElement>) => {
    drag.current = { x: ev.clientX, y: ev.clientY }
    ev.currentTarget.setPointerCapture(ev.pointerId)
  }
  const onMove = (ev: React.PointerEvent<SVGSVGElement>) => {
    const d = drag.current
    if (!d) return
    // 一个球宽 = 180°，按这个比例换算拖动距离
    const k = 180 / (R * 2)
    setLambda((l) => l + (ev.clientX - d.x) * k)
    setPhi((p) => Math.max(-89, Math.min(89, p - (ev.clientY - d.y) * k)))
    drag.current = { x: ev.clientX, y: ev.clientY }
  }
  const onUp = (ev: React.PointerEvent<SVGSVGElement>) => {
    drag.current = null
    ev.currentTarget.releasePointerCapture(ev.pointerId)
  }

  return (
    // **不装在框里**。参照站的地球是一个大幅溢出的装饰元素：
    // 向右下伸出去，卡片半透明地压在它上面。装进 panel 里就变成一个
    // 小挂件，和「统计块要多高地球就多高」这类对齐问题也一并没有了。
    //
    // 定位交给调用方（`className`）：宽屏时绝对定位到右上角，
    // 窄屏时回到正常流里居中缩小 —— 同一个实例，只换 CSS，
    // 不必为两种布局各渲染一个（那会跑两个 rAF 循环）。
    <div className={`relative ${className}`}>
      {geo && (
        <div className="relative aspect-square w-full">
          <svg
            viewBox={`0 0 ${SIZE} ${SIZE}`}
            className="absolute inset-0 m-auto h-full max-h-full w-auto max-w-full cursor-grab touch-none select-none active:cursor-grabbing"
            role="img"
            aria-label={`服务器分布于 ${legend.length} 个国家`}
            onPointerDown={onDown}
            onPointerMove={onMove}
            onPointerUp={onUp}
            onPointerCancel={onUp}
            onMouseEnter={() => (spinning.current = false)}
            onMouseLeave={() => {
              spinning.current = true
              setHover(null)
            }}
          >
            {/* 海洋。渐变让球看起来是个球而不是一个圆片 */}
            <defs>
              <radialGradient id="ocean" cx="35%" cy="30%" r="75%">
                <stop offset="0%" stopColor="var(--globe-sea-hi)" />
                <stop offset="100%" stopColor="var(--globe-sea-lo)" />
              </radialGradient>
            </defs>
            <circle cx={R + PAD} cy={R + PAD} r={R} fill="url(#ocean)" />

            {/* 经纬网。没有它眼睛读不出这是个转动的球 */}
            <g fill="none" stroke="var(--globe-grid)" strokeWidth={0.5}>
              {[-60, -30, 0, 30, 60].map((lat) => (
                <path key={`p${lat}`} d={parallel(lat, view, b)} />
              ))}
              {[0, 30, 60, 90, 120, 150, 180, 210, 240, 270, 300, 330].map((lon) => (
                <path key={`m${lon}`} d={meridian(lon, view, b)} />
              ))}
            </g>

            {paths.map(([cc, d]) => {
              const stat = byCountry.get(cc)
              return (
                <path
                  key={cc}
                  d={d}
                  className={
                    !stat
                      ? 'fill-slate-200/70 dark:fill-white/20'
                      : stat.online > 0
                        ? 'fill-emerald-500 hover:fill-emerald-400'
                        : 'fill-red-500 hover:fill-red-400'
                  }
                  stroke="var(--globe-edge)"
                  strokeWidth={stat ? 0.8 : 0.3}
                  onMouseEnter={() => stat && setHover(cc)}
                >
                  {stat && <title>{`${cc}：在线 ${stat.online} / 共 ${stat.total}`}</title>}
                </path>
              )
            })}

            {/* 小国的标记点 */}
            {dots.map(([cc, x, y]) => {
              const stat = byCountry.get(cc)!
              return (
                <g key={`dot-${cc}`} onMouseEnter={() => setHover(cc)}>
                  <circle
                    cx={x}
                    cy={y}
                    r={4}
                    className={stat.online > 0 ? 'fill-emerald-500' : 'fill-red-500'}
                    stroke="var(--globe-edge)"
                    strokeWidth={1}
                  />
                  <title>{`${cc}：在线 ${stat.online} / 共 ${stat.total}`}</title>
                </g>
              )
            })}

            {/* 边缘一圈，把球收住 */}
            <circle cx={R + PAD} cy={R + PAD} r={R} fill="none" stroke="var(--globe-rim)" strokeWidth={1} />
          </svg>

          {/* 悬停提示：把国家和数量直接说出来 */}
          {hover && hoverStat && (
            <div className="pointer-events-none absolute left-2 top-2 flex items-center gap-1.5 rounded-lg border border-black/10 bg-white/95 px-2 py-1 text-xs shadow dark:border-white/15 dark:bg-zinc-900/95">
              <Flag code={hover} />
              <span className="tabular-nums">
                在线 <span className="text-emerald-600 dark:text-emerald-400">{hoverStat.online}</span>
                {' / '}共 {hoverStat.total}
              </span>
            </div>
          )}
        </div>
      )}

      {failed && (
        <p className="absolute inset-x-0 bottom-0 text-center text-xs text-zinc-500">
          地图数据加载失败
        </p>
      )}
    </div>
  )
}

/**
 * 地球旁边的图例：在线 / 离线台数 + 每个国家的国旗和数量。
 *
 * 和地球本体分开导出，因为地球是压在卡片**底下**的装饰层，
 * 图例得浮在上面才点得到、看得清。
 */
export function GlobeLegend({
  servers,
  className = '',
}: {
  servers: ServerEntry[]
  className?: string
}) {
  const online = servers.filter((e) => e.server.online).length
  const offline = servers.length - online
  const byCountry = new Map<string, CountryStat>()
  for (const e of servers) {
    const c = e.server.country_code?.toUpperCase()
    if (!c) continue
    const cur = byCountry.get(c) ?? { total: 0, online: 0 }
    cur.total++
    if (e.server.online) cur.online++
    byCountry.set(c, cur)
  }
  const legend = [...byCountry.entries()].sort((a, b) => b[1].total - a[1].total)
  if (servers.length === 0) return null

  return (
    <div className={`flex flex-wrap items-center gap-x-3 gap-y-1.5 text-xs ${className}`}>
      <span className="flex items-center gap-2 rounded-full bg-white/70 px-2.5 py-1 tabular-nums shadow-sm backdrop-blur dark:bg-white/10">
        <span className="flex items-center gap-1 text-emerald-600 dark:text-emerald-400">
          <span className="inline-block h-1.5 w-1.5 rounded-full bg-current" />
          {online}
        </span>
        {offline > 0 && (
          <span className="flex items-center gap-1 text-zinc-400">
            <span className="inline-block h-1.5 w-1.5 rounded-full bg-current" />
            {offline}
          </span>
        )}
      </span>
      {legend.map(([code, n]) => (
        <span
          key={code}
          className="flex items-center gap-1 rounded-full bg-white/70 px-2 py-1 shadow-sm backdrop-blur dark:bg-white/10"
        >
          <Flag code={code} />
          <span className="tabular-nums">
            <span className="text-emerald-600 dark:text-emerald-400">{n.online}</span>
            <span className="text-zinc-400">/{n.total}</span>
          </span>
        </span>
      ))}
    </div>
  )
}

/** 一条纬线。只画朝向我们这半边。 */
function parallel(lat: number, view: View, b: ReturnType<typeof basis>): string {
  return arc((lon) => [lon, lat], 0, 360, 6, view, b)
}

/** 一条经线。 */
function meridian(lon: number, view: View, b: ReturnType<typeof basis>): string {
  return arc((lat) => [lon, lat], -90, 90, 4, view, b)
}

/** 采样一条曲线，跳过转到背面的部分（断开成多段）。 */
function arc(
  at: (t: number) => LonLat,
  from: number,
  to: number,
  step: number,
  view: View,
  b: ReturnType<typeof basis>,
): string {
  let d = ''
  let pen = false
  for (let t = from; t <= to; t += step) {
    const p = projectLonLat(at(t), view, b)
    if (p.depth <= 0) {
      pen = false
      continue
    }
    d += `${pen ? 'L' : 'M'}${p.x.toFixed(1)},${p.y.toFixed(1)}`
    pen = true
  }
  return d
}
