import type { ServerEntry, Summary } from '../api'
import * as f from '../lib/format'
import { IconChart, IconDisk, IconDown, IconMemory, IconUp, IconWallet } from './icons'

/**
 * 顶部统计（R12 + R15）。
 *
 * 布局参照 monitor.jqwebs.cc：一排卡片，每张一个指标 + 一句小字。
 * 比原来一行挤七个数字更好扫。
 *
 * **「N 台未填价格」这类提示不再常驻**。用户明确说过不想看它 ——
 * 而且价格为 0 现在就是「不公开价格」的合法取值，不是遗漏。
 * 真正会让数字失真的两项（缺汇率、汇率过期）仍然保留：
 * 那是「这个数算不准」，不说才是撒谎。
 */
export function StatBar({ s, servers }: { s: Summary | null; servers: ServerEntry[] }) {
  if (!s) {
    return <div className="h-24 animate-pulse rounded-xl bg-black/5 dark:bg-white/5" />
  }
  const v = s.value

  // 只保留会让金额失真的提示。「未填价格」是用户的选择，不是问题
  const warn = [
    v.no_rate_servers > 0 && `${v.no_rate_servers} 台的货币没有汇率，未计入金额`,
    // 「未设到期」原本单独占一张卡，但它和上面那几个是同一类东西：
    // **会让金额算不准**。归到提示行里，六张卡只放数字
    v.no_expire_servers > 0 && `${v.no_expire_servers} 台没设到期时间，未计入剩余价值`,
    s.rate.stale && `汇率停留在 ${s.rate.as_of}，可能已过期`,
  ].filter(Boolean) as string[]

  // 内存与硬盘的总量/已用，跨全部在线机器求和
  let memUsed = 0
  let memTotal = 0
  let diskUsed = 0
  let diskTotal = 0
  let trafficIn = 0
  let trafficOut = 0
  for (const e of servers) {
    if (e.server.online) {
      memUsed += e.server.mem.used
      memTotal += e.server.mem.total
      diskUsed += e.server.disk.used
      diskTotal += e.server.disk.total
    }
    trafficIn += e.traffic?.in_bytes ?? 0
    trafficOut += e.traffic?.out_bytes ?? 0
  }

  return (
    // 固定 3 列 × 2 行。参照站也是这个形状 —— 和右边的地球并排时，
    // 高度正好凑得上，而且六个数字一眼扫得完，不用上下找。
    // **不要**让它跟着地球拉伸（试过 flex-1 + auto-rows-fr）：
    // 卡片会被撑成一个个大空盒子，比留白还难看。
    <section className="space-y-2">
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
        <Card
          icon={<IconMemory className="text-zinc-400 dark:text-zinc-500" />}
          label="内存用量"
          value={f.bytes(memUsed)}
          unit={memTotal > 0 ? `/ ${f.bytes(memTotal)}` : ''}
          sub={memTotal > 0 ? f.pct((memUsed / memTotal) * 100) : '—'}
        />
        {/* 剩余价值放中间：它是六个里唯一「和钱有关」的，
            总价值和年化跟着它做副标题，不再单独占一张卡 */}
        <Card
          icon={<IconWallet className="text-zinc-400 dark:text-zinc-500" />}
          label="剩余价值"
          value={f.money(v.total_remaining, v.display_currency)}
          sub={`共 ${f.money(v.total_value, v.display_currency)} · 年化 ${f.money(v.annual_cost, v.display_currency)}`}
        />
        <Card
          icon={<IconUp className="text-zinc-400 dark:text-zinc-500" />}
          label="实时上行"
          value={f.speed(s.network.out_speed)}
          sub="全部节点合计"
        />
        <Card
          icon={<IconDisk className="text-zinc-400 dark:text-zinc-500" />}
          label="硬盘用量"
          value={f.bytes(diskUsed)}
          unit={diskTotal > 0 ? `/ ${f.bytes(diskTotal)}` : ''}
          sub={diskTotal > 0 ? f.pct((diskUsed / diskTotal) * 100) : '—'}
        />
        <Card
          icon={<IconChart className="text-zinc-400 dark:text-zinc-500" />}
          label="累计流量"
          value={f.bytes(trafficIn + trafficOut)}
          sub={`↑${f.bytes(trafficOut, 0)} ↓${f.bytes(trafficIn, 0)}`}
        />
        <Card
          icon={<IconDown className="text-zinc-400 dark:text-zinc-500" />}
          label="实时下行"
          value={f.speed(s.network.in_speed)}
          sub="全部节点合计"
        />
      </div>

      {warn.length > 0 && (
        <p className="text-xs text-amber-600 dark:text-amber-500">{warn.join(' · ')}</p>
      )}
    </section>
  )
}

function Card({
  icon,
  label,
  value,
  unit,
  sub,
  tone,
}: {
  icon?: React.ReactNode
  label: string
  value: string
  unit?: string
  sub?: string
  tone?: 'ok' | 'warn'
}) {
  const color =
    tone === 'ok' ? 'text-emerald-600 dark:text-emerald-400'
    : tone === 'warn' ? 'text-amber-600 dark:text-amber-500'
    : ''
  return (
    // p-4 + 大一号的数字：参照站的统计卡也是这个分量。
    // 顺带把这一列的自然高度顶上去，和右边的地球差不多齐
    <div className="panel p-4">
      <div className="flex items-start justify-between gap-2">
        <span className="text-xs text-zinc-500">{label}</span>
        {icon}
      </div>
      <div className="mt-1.5 flex items-baseline gap-1">
        <span className={`text-2xl font-semibold tabular-nums ${color}`}>{value}</span>
        {unit && <span className="text-xs text-zinc-500 tabular-nums">{unit}</span>}
      </div>
      {sub && <div className="mt-0.5 truncate text-[11px] text-zinc-500 tabular-nums">{sub}</div>}
    </div>
  )
}
