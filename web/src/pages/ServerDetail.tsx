import { useEffect, useMemo, useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { api, type ServerEntry, type Series } from '../api'
import * as f from '../lib/format'
import { Chart } from '../components/Chart'
import { alignSeries, lineColor } from '../lib/align'
import { Flag } from '../components/Flag'
import { OsIcon } from '../components/OsIcon'

/**
 * 单机详情页（R16 展开）。
 *
 * 结构参照 monitor.jqwebs.cc：
 * 顶部一排统计卡 → 硬件 / 系统 / 存储 / 网络四张信息卡 → 时间范围 → 图表 → 延迟监测。
 *
 * 一条贯穿的规则：**采不到的东西整块不显示，不显示 0 也不显示占位符**。
 * 一个写着「0」的图表会让人以为这台机器真的没有进程、没有连接。
 */
const RANGES = [
  ['实时', '1h'],
  ['6 小时', '6h'],
  ['1 天', '24h'],
  ['7 天', '7d'],
  ['30 天', '30d'],
  ['1 年', '1y'],
] as const

/** 一个延迟探测点在当前时间范围内的数据。 */
interface Ping {
  id: number
  name: string
  s: Series
}

export default function ServerDetail() {
  const { uuid = '' } = useParams()
  const [entry, setEntry] = useState<ServerEntry | null>(null)
  const [currency, setCurrency] = useState('CNY')
  const [range, setRange] = useState<string>('6h')
  const [m, setM] = useState<Series | null>(null)
  const [pings, setPings] = useState<Ping[]>([])
  const [err, setErr] = useState<string | null>(null)

  useEffect(() => {
    api.servers().then((r) => {
      setEntry(r.servers.find((e) => e.server.id === uuid) ?? null)
      setCurrency(r.display_currency)
    })
  }, [uuid])

  useEffect(() => {
    let alive = true
    setErr(null)
    api
      .metrics(uuid, range)
      .then((a) => alive && setM(a))
      .catch((e) => alive && setErr(e instanceof Error ? e.message : String(e)))
    // 先问有哪些延迟任务，再逐个取数据。
    // 之前是盲猜 task_id 1..5 各请求一次 —— 猜不到名字（只能显示「延迟监测 #1」），
    // 任务超过 5 个还会漏，而且每次都要发 5 个必然有几个 404 的请求。
    void (async () => {
      let tasks: { id: number; name: string }[] = []
      try {
        tasks = await api.pingTasks(uuid)
      } catch {
        // 一个都没配、或者接口不可用 —— 整块不显示，不是错误
      }
      const rs = await Promise.all(
        tasks.map(async (t): Promise<Ping | null> => {
          try {
            const s = await api.ping(uuid, range, t.id)
            return s.ts.length > 0 ? { id: t.id, name: t.name, s } : null
          } catch {
            return null
          }
        }),
      )
      if (alive) setPings(rs.filter((x): x is Ping => x !== null))
    })()
    return () => {
      alive = false
    }
  }, [uuid, range])

  const s = entry?.server
  const b = entry?.billing
  const t = entry?.traffic
  const col = useMemo(
    () => (k: string) => ((m?.[k] as (number | null)[] | undefined) ?? []).slice(),
    [m],
  )

  if (!entry || !s) {
    return <p className="text-sm text-zinc-500">{err ?? '加载中…'}</p>
  }

  const ts = m?.ts ?? []

  // 各探测点分别查出来的，画到一张图上要先对齐时间轴
  const latency = alignSeries(
    pings.map((p) => ({
      ts: p.s.ts,
      // 库里存的是微秒，图上要毫秒
      values: ((p.s.rtt_avg as (number | null)[] | undefined) ?? []).map((v) =>
        v == null ? null : v / 1000,
      ),
    })),
  )
  const loss = alignSeries(
    pings.map((p) => ({
      ts: p.s.ts,
      values: ((p.s.loss_pct as (number | null)[] | undefined) ?? []).slice(),
    })),
  )
  const hasLoad = s.capabilities.load_average && col('load1').some((v) => v != null)
  const hasConn = s.capabilities.tcp_conn_count && col('tcp_conn').some((v) => v != null)
  const hasProc = s.capabilities.proc_count && col('proc_count').some((v) => v != null)
  const swap = col('swap_used')
  const hasSwap = s.mem.swap_total > 0 && swap.some((v) => v != null)

  return (
    <div className="space-y-4">
      {/* ── 顶部：返回 / 国旗 / 名称 / 状态 ── */}
      <div className="flex flex-wrap items-center gap-2">
        <Link to="/" className="text-sm text-zinc-500 hover:underline">
          ← 返回
        </Link>
        <Flag code={s.country_code} className="text-lg" />
        <h1 className="text-lg font-semibold">{s.name}</h1>
        <span
          className={`rounded px-1.5 py-0.5 text-xs ${
            s.online
              ? 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-400'
              : 'bg-red-500/15 text-red-600 dark:text-red-400'
          }`}
        >
          {s.online ? '在线' : s.last_seen === 0 ? '未安装探针' : '离线'}
        </span>
        {s.buy_url && (
          <Ext href={s.buy_url}>🛒 购买同款</Ext>
        )}
        {s.review_url && <Ext href={s.review_url}>📝 测评</Ext>}
      </div>

      {/* ── 统计卡 ── */}
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
        <Stat label="节点价格" value={f.price(b, currency)} />
        <Stat label="剩余时间" value={remainText(b)} tone={remainTone(b)} />
        <Stat
          label="剩余价值"
          value={b && !b.infinite && !b.unknown_expiry ? f.money(b.remaining_display, currency) : '—'}
        />
        <Stat label="运行时间" value={s.online ? f.duration(s.uptime_s) : '—'} />
        <Stat label="累计流量" value={t ? f.bytes(t.used) : '—'} sub={t ? `配额 ${f.limit(t.limit)}` : ''} />
        <Stat
          label="流量用量"
          value={t?.pct != null ? f.pct(t.pct) : '∞'}
          sub={t ? `↑${f.bytes(t.out_bytes, 0)} ↓${f.bytes(t.in_bytes, 0)}` : ''}
        />
        <Stat
          label="实时网速"
          value={s.online ? `↑${f.speed(s.net.tx_speed)}` : '—'}
          sub={s.online ? `↓${f.speed(s.net.rx_speed)}` : ''}
        />
        <Stat
          label="连接数"
          value={hasConn && s.tcp_conn != null ? String(s.tcp_conn) : '—'}
          sub={hasProc && s.proc_count != null ? `${s.proc_count} 进程` : ''}
        />
      </div>

      {/* ── 信息卡 ── */}
      <div className="grid gap-2 lg:grid-cols-2">
        <Panel title="硬件信息">
          <Row k="CPU" v={s.cpu_model ?? '—'} />
          <div className="grid grid-cols-3 gap-2">
            <Row k="架构" v={s.arch || '—'} />
            <Row k="核心" v={s.cpu_cores > 0 ? `${s.cpu_cores} 核` : '—'} />
            <Row k="虚拟化" v={virtText(s.virtualization)} />
          </div>
        </Panel>
        <Panel title="系统信息">
          <Row
            k="操作系统"
            v={
              <span className="flex items-center gap-1.5">
                <OsIcon os={s.os} />
                {s.os || '—'}
              </span>
            }
          />
          <div className="grid grid-cols-2 gap-2">
            <Row k="内核" v={s.kernel ?? '—'} />
            <Row k="探针版本" v={s.agent_version || '—'} />
          </div>
        </Panel>
        <Panel title="存储信息">
          <div className="grid grid-cols-3 gap-2">
            <Row k="内存" v={s.mem.total > 0 ? f.bytes(s.mem.total) : '—'} />
            <Row k="交换分区" v={s.mem.swap_total > 0 ? f.bytes(s.mem.swap_total) : '无'} />
            <Row k="硬盘" v={s.disk.total > 0 ? f.bytes(s.disk.total) : '—'} />
          </div>
        </Panel>
        <Panel title="网络信息">
          <div className="grid grid-cols-2 gap-2">
            <Row k="网卡" v={s.net.ifaces.join(', ') || '—'} />
            <Row
              k="总流量"
              v={t ? `${f.bytes(t.used)} / ${f.limit(t.limit)}` : '—'}
            />
          </div>
        </Panel>
      </div>

      {/* ── 时间范围 ── */}
      <nav className="flex flex-wrap gap-1" aria-label="时间范围">
        {RANGES.map(([label, r]) => (
          <button
            key={r}
            onClick={() => setRange(r)}
            aria-current={range === r ? 'true' : undefined}
            className={`rounded-lg px-2.5 py-1 text-sm transition ${
              range === r
                ? 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-400'
                : 'text-zinc-500 hover:bg-black/5 dark:hover:bg-white/10'
            }`}
          >
            {label}
          </button>
        ))}
        {m?.granularity && (
          <span className="ml-auto self-center text-xs text-zinc-400">
            数据粒度 {m.granularity} · {ts.length} 个点
          </span>
        )}
      </nav>

      {err && <p className="text-sm text-red-500">{err}</p>}

      {/* ── 图表 ── */}
      <div className="grid gap-3 xl:grid-cols-2">
        <ChartCard title="CPU 与负载">
          <Chart
            ts={ts}
            lines={[
              { label: 'CPU %', data: col('cpu_pct').map((v) => (v == null ? null : v / 100)), color: '#f59e0b' },
              ...(hasLoad
                ? [{ label: '负载', data: col('load1').map((v) => (v == null ? null : v / 100)), color: '#8b5cf6' }]
                : []),
            ]}
          />
        </ChartCard>

        <ChartCard title={hasSwap ? '内存与 Swap' : '内存'}>
          <Chart
            ts={ts}
            lines={[
              {
                label: '已用',
                data: col('mem_total').map((tot, i) => {
                  const av = col('mem_available')[i]
                  return tot == null || av == null ? null : (tot - av) / 1048576
                }),
                color: '#10b981',
              },
              ...(hasSwap
                ? [{ label: 'Swap', data: swap.map((v) => (v == null ? null : v / 1048576)), color: '#a855f7' }]
                : []),
            ]}
          />
        </ChartCard>

        <ChartCard title="磁盘">
          <Chart
            ts={ts}
            lines={[
              {
                label: '已用 GiB',
                data: col('disk_used').map((v) => (v == null ? null : v / 1073741824)),
                color: '#0ea5e9',
              },
            ]}
          />
        </ChartCard>

        <ChartCard title="网络速率">
          <Chart
            ts={ts}
            lines={[
              {
                label: '下行 KiB/s',
                data: col('net_in_speed').map((v) => (v == null ? null : v / 1024)),
                color: '#0ea5e9',
              },
              {
                label: '上行 KiB/s',
                data: col('net_out_speed').map((v) => (v == null ? null : v / 1024)),
                color: '#10b981',
              },
            ]}
          />
        </ChartCard>

        {/* 采不到就整块不出现 —— 画一条恒为 0 的线会让人以为真的没有连接 */}
        {hasConn && (
          <ChartCard title="网络连接">
            <Chart
              ts={ts}
              lines={[
                { label: 'TCP', data: col('tcp_conn'), color: '#ef4444' },
                { label: 'UDP', data: col('udp_conn'), color: '#14b8a6' },
              ]}
            />
          </ChartCard>
        )}

        {hasProc && (
          <ChartCard title="进程数">
            <Chart ts={ts} lines={[{ label: '进程', data: col('proc_count'), color: '#8b5cf6' }]} />
          </ChartCard>
        )}
      </div>

      {/* ── 延迟监测 ── */}
      {/* 延迟与丢包：**所有探测点画在同一张图上**，一个探测点一种颜色。
          之前是一个任务一张图，两三个探测点就要来回扫好几张图才能比出
          「哪个线路更慢」—— 而这正是配多个探测点的唯一理由。
          各任务的时间戳落在同一个分桶网格上，所以 `alignSeries` 是精确对齐。 */}
      {pings.length > 0 && (
        <div className="grid gap-3 xl:grid-cols-2">
          <ChartCard title="延迟">
            <Chart
              ts={latency.ts}
              lines={pings.map((p, i) => ({
                label: p.name,
                data: latency.cols[i] ?? [],
                color: lineColor(i),
                fmt: (v) => (v == null ? '—' : `${v.toFixed(1)} ms`),
              }))}
            />
          </ChartCard>
          <ChartCard title="丢包">
            <Chart
              ts={loss.ts}
              lines={pings.map((p, i) => ({
                label: p.name,
                data: loss.cols[i] ?? [],
                color: lineColor(i),
                fmt: (v) => (v == null ? '—' : `${v.toFixed(0)}%`),
              }))}
            />
          </ChartCard>
        </div>
      )}
    </div>
  )
}

function Stat({ label, value, sub, tone }: { label: string; value: string; sub?: string; tone?: string }) {
  return (
    <div className="panel p-3">
      <div className="text-xs text-zinc-500">{label}</div>
      <div className={`mt-1 text-lg font-semibold tabular-nums ${tone ?? ''}`}>{value}</div>
      {sub && <div className="mt-0.5 truncate text-[11px] text-zinc-500 tabular-nums">{sub}</div>}
    </div>
  )
}

function Panel({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="panel p-3">
      <h2 className="mb-2 text-sm font-medium">{title}</h2>
      <div className="space-y-2">{children}</div>
    </section>
  )
}

function Row({ k, v }: { k: string; v: React.ReactNode }) {
  return (
    <div className="min-w-0">
      <div className="text-xs text-zinc-500">{k}</div>
      <div className="truncate text-sm">{v}</div>
    </div>
  )
}

function ChartCard({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="panel p-3">
      <h2 className="mb-1 text-sm font-medium">{title}</h2>
      {children}
    </section>
  )
}

function Ext({ href, children }: { href: string; children: React.ReactNode }) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noreferrer noopener"
      className="rounded bg-sky-500/10 px-1.5 py-0.5 text-xs text-sky-700 hover:bg-sky-500/20 dark:text-sky-400"
    >
      {children}
    </a>
  )
}

function remainText(b: ServerEntry['billing'] | undefined): string {
  if (!b) return '—'
  if (b.infinite) return '长期'
  if (b.unknown_expiry || b.remain_days == null) return '未设到期'
  return b.remain_days < 0 ? `已过期 ${-b.remain_days} 天` : `${b.remain_days} 天`
}

function remainTone(b: ServerEntry['billing'] | undefined): string {
  if (!b || b.infinite || b.unknown_expiry || b.remain_days == null) return ''
  if (b.remain_days < 0) return 'text-red-500'
  if (b.remain_days <= 7) return 'text-amber-600 dark:text-amber-400'
  return ''
}

/** `vm` / `none` 是「认不出」和「物理机」，显示出来没有信息量。 */
function virtText(v: string | null): string {
  return v && !['none', 'vm', ''].includes(v) ? v : '—'
}
