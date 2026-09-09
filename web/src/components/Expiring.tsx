import { useEffect, useRef, useState } from 'react'
import { Link } from 'react-router-dom'
import type { ExpiringItem } from '../api'
import * as f from '../lib/format'
import { Flag } from './Flag'

/**
 * 即将到期（R13）。
 *
 * **已过期的也列出来**（剩余天数为负、排最前）——
 * 一台悄悄过期的机器正是最需要被看见的。
 *
 * 多条时做成堆叠轮播（iPhone 智慧堆叠那种）而不是列表：
 * 这块紧挨在汇总卡下面，列出五六行会把服务器卡片推到屏幕外，
 * 而它本来只是个提醒 —— 要处理的话点进去处理，不需要在首页摊开。
 */
const ROTATE_MS = 4000

export function Expiring({ items, currency }: { items: ExpiringItem[]; currency: string }) {
  const [at, setAt] = useState(0)
  const hover = useRef(false)
  const n = items.length

  useEffect(() => {
    if (n <= 1) return
    // 用户关了动效就不自动轮播 —— 下面的翻页点仍然能手动切
    if (window.matchMedia?.('(prefers-reduced-motion: reduce)').matches) return
    const t = setInterval(() => {
      if (!hover.current) setAt((i) => (i + 1) % n)
    }, ROTATE_MS)
    return () => clearInterval(t)
  }, [n])

  // 机器数变少时（比如续费了）当前下标可能越界
  useEffect(() => {
    if (at >= n) setAt(0)
  }, [at, n])

  if (n === 0) return null
  const cur = items[Math.min(at, n - 1)]!

  return (
    <section
      className="panel p-4"
      onMouseEnter={() => (hover.current = true)}
      onMouseLeave={() => (hover.current = false)}
    >
      <div className="mb-2 flex items-center justify-between gap-2">
        <h2 className="text-sm font-medium">
          即将到期
          {n > 1 && <span className="ml-1.5 text-xs font-normal text-zinc-500">{n} 台</span>}
        </h2>
        {n > 1 && (
          <div className="flex items-center gap-1" role="tablist" aria-label="切换">
            {items.map((it, i) => (
              <button
                key={it.id}
                role="tab"
                aria-selected={i === at}
                aria-label={it.name}
                onClick={() => setAt(i)}
                className={`h-1.5 rounded-full transition-all ${
                  i === at ? 'w-4 bg-zinc-400 dark:bg-zinc-300' : 'w-1.5 bg-zinc-300 dark:bg-zinc-600'
                }`}
              />
            ))}
          </div>
        )}
      </div>

      {/* 堆叠：后面两张露出一点边，暗示「还有」。
          高度固定，切换时整块不会跳。 */}
      <div className="relative h-11">
        {n > 1 && (
          <>
            <div className="panel absolute inset-x-3 top-1.5 h-11 opacity-60" aria-hidden />
            {n > 2 && <div className="panel absolute inset-x-6 top-3 h-11 opacity-30" aria-hidden />}
          </>
        )}
        <Row key={cur.id} item={cur} currency={currency} />
      </div>
    </section>
  )
}

function Row({ item: i, currency }: { item: ExpiringItem; currency: string }) {
  const tone =
    i.remain_days < 0 ? 'text-red-500'
    : i.remain_days <= 3 ? 'text-amber-500'
    : ''
  return (
    // `animate-fade` 让切换有个淡入，不是硬闪。key 变化时 React 重建这个节点，
    // 动画自然重放
    <div className="animate-fade panel absolute inset-x-0 top-0 flex h-11 items-center gap-2.5 px-3 text-sm">
      <Flag code={i.country_code} className="shrink-0 text-sm" />
      <Link to={`/server/${i.id}`} className="truncate font-medium hover:underline">
        {i.name}
      </Link>
      {i.renew_state === 'renewing' && (
        <span
          className="shrink-0 rounded bg-amber-500/15 px-1.5 py-0.5 text-[11px] text-amber-600 dark:text-amber-400"
          title="已开启自动续费，仍在宽限期内"
        >
          续费中
        </span>
      )}
      <span className={`ml-auto shrink-0 tabular-nums ${tone}`}>{f.remainDays(i.remain_days)}</span>
      <span className="w-20 shrink-0 text-right text-xs tabular-nums text-zinc-500">
        {f.money(i.remaining_display, currency)}
      </span>
    </div>
  )
}
