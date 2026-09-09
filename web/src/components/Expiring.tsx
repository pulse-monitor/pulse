import { Link } from 'react-router-dom'
import type { ExpiringItem } from '../api'
import * as f from '../lib/format'
import { Flag } from './Flag'

/**
 * 即将到期（R13）。
 *
 * **已过期的也列出来**（剩余天数为负、排最前）——
 * 一台悄悄过期的机器正是最需要被看见的。
 */
export function Expiring({
  items,
  currency,
}: {
  items: ExpiringItem[]
  currency: string
}) {
  if (items.length === 0) return null
  return (
    <section className="rounded-xl border border-black/10 bg-white p-4 dark:border-white/10 dark:bg-white/5">
      <h2 className="mb-2 text-sm font-medium">即将到期</h2>
      <ul className="space-y-1.5 text-sm">
        {items.map((i) => (
          <li key={i.id} className="flex items-center gap-2">
            <Flag code={i.country_code} className="text-sm" />
            <Link to={`/server/${i.id}`} className="truncate hover:underline">
              {i.name}
            </Link>
            <span
              className={`ml-auto shrink-0 tabular-nums ${
                i.remain_days < 0 ? 'text-red-500'
                : i.remain_days <= 3 ? 'text-amber-500'
                : ''
              }`}
            >
              {f.remainDays(i.remain_days)}
            </span>
            {i.renew_state === 'renewing' && (
              <span className="shrink-0 text-xs text-amber-500" title="已开启自动续费，仍在宽限期内">
                续费中
              </span>
            )}
            <span className="w-16 shrink-0 text-right text-xs text-zinc-500">
              {f.money(i.remaining_display, currency)}
            </span>
          </li>
        ))}
      </ul>
    </section>
  )
}
