import { Link } from 'react-router-dom'
import type { ServerEntry } from '../api'
import * as f from '../lib/format'
import { Bar } from './Bar'
import { Flag } from './Flag'
import { OsIcon } from './OsIcon'

/**
 * 列表视图的一行。
 *
 * 和卡片是同一份数据的两种呈现：卡片适合十来台时一眼扫完，
 * 列表适合几十上百台时按列比较（哪台 CPU 最高、哪台快到期）。
 *
 * 窄屏下横向滚动而不是折行 —— 列表的价值就在于「同一列上下对齐」，
 * 一折行这个价值就没了。
 */
export function ServerRow({ e, currency }: { e: ServerEntry; currency: string }) {
  const s = e.server
  const t = e.traffic
  const b = e.billing
  const live = s.online

  return (
    <tr className="border-t border-black/5 hover:bg-black/[0.02] dark:border-white/10 dark:hover:bg-white/[0.03]">
      <td className="whitespace-nowrap py-1.5 pr-3">
        <span className="flex items-center gap-1.5">
          <span
            className={`${live ? 'dot-on text-emerald-500' : 'dot-off text-zinc-400'} text-[9px]`}
            aria-label={live ? '在线' : '离线'}
          />
          <Flag code={s.country_code} />
          <OsIcon os={s.os} />
          <Link to={`/server/${s.id}`} className="font-medium hover:text-emerald-600">
            {s.name}
          </Link>
        </span>
      </td>
      <Cell>{live ? f.duration(s.uptime_s) : s.last_seen === 0 ? '未安装' : '离线'}</Cell>
      <Metric pct={live ? s.cpu_pct : null} text={live ? f.pct(s.cpu_pct) : '—'} />
      <Metric
        pct={live && s.mem.total > 0 ? s.mem.pct : null}
        text={live && s.mem.total > 0 ? f.pct(s.mem.pct) : '—'}
      />
      <Metric
        pct={live && s.disk.total > 0 ? s.disk.pct : null}
        text={live && s.disk.total > 0 ? f.pct(s.disk.pct) : '—'}
      />
      <Cell>
        <span className="text-emerald-600 dark:text-emerald-400">↑{f.speed(live ? s.net.tx_speed : 0)}</span>{' '}
        <span className="text-sky-600 dark:text-sky-400">↓{f.speed(live ? s.net.rx_speed : 0)}</span>
      </Cell>
      <Cell>{t ? `${f.bytes(t.used, 0)} / ${f.limit(t.limit)}` : '—'}</Cell>
      <Cell>{f.price(b, currency)}</Cell>
      <Cell tone={remainTone(b)}>{remainText(b)}</Cell>
    </tr>
  )
}

function Cell({ children, tone }: { children: React.ReactNode; tone?: string }) {
  return <td className={`whitespace-nowrap py-1.5 pr-3 tabular-nums ${tone ?? ''}`}>{children}</td>
}

/** 百分比列：数字 + 一条窄进度条，扫一眼就能比较。 */
function Metric({ pct, text }: { pct: number | null; text: string }) {
  return (
    <td className="whitespace-nowrap py-1.5 pr-3">
      <span className="flex items-center gap-1.5">
        <span className="w-11 text-right tabular-nums">{text}</span>
        <Bar pct={pct} className="w-12" />
      </span>
    </td>
  )
}

function remainText(b: ServerEntry['billing']): string {
  if (!b) return '—'
  if (b.infinite) return '长期'
  if (b.unknown_expiry || b.remain_days == null) return '—'
  return b.remain_days < 0 ? `已过期 ${-b.remain_days} 天` : `${b.remain_days} 天`
}

function remainTone(b: ServerEntry['billing']): string {
  if (!b || b.infinite || b.unknown_expiry || b.remain_days == null) return 'text-zinc-500'
  if (b.remain_days < 0) return 'text-red-500'
  if (b.remain_days <= 7) return 'text-amber-600 dark:text-amber-400'
  return ''
}
