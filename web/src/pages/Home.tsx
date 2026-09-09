import { useEffect, useMemo, useState } from 'react'
import { api, type ExpiringItem, type Group, type ServerEntry, type Summary } from '../api'
import { Live, type Tick } from '../lib/live'
import { Expiring } from '../components/Expiring'
import { Globe, GlobeLegend } from '../components/Globe'
import { ServerCard } from '../components/ServerCard'
import { ServerRow } from '../components/ServerRow'
import { StatBar } from '../components/StatBar'

const REFRESH_MS = 60_000

const VIEW_KEY = 'pulse.view'

/**
 * 卡片 / 列表切换。
 *
 * 两种视图各有各的场合：十来台时卡片一眼扫完，几十上百台时
 * 列表能按列比较（谁 CPU 最高、谁快到期）。偏好记在本地。
 */
function ViewToggle({
  view,
  onChange,
}: {
  view: 'card' | 'list'
  onChange: (v: 'card' | 'list') => void
}) {
  const set = (v: 'card' | 'list') => {
    onChange(v)
    try {
      localStorage.setItem(VIEW_KEY, v)
    } catch {
      /* 隐私模式下存不了，本次会话内生效即可 */
    }
  }
  return (
    <span className="ml-auto flex items-center gap-0.5 rounded-lg bg-black/5 p-0.5 dark:bg-white/10">
      {(
        [
          ['card', '卡片', '▦'],
          ['list', '列表', '☰'],
        ] as const
      ).map(([v, label, icon]) => (
        <button
          key={v}
          onClick={() => set(v)}
          aria-pressed={view === v}
          title={`${label}视图`}
          className={`rounded px-2 py-0.5 text-sm transition ${
            view === v
              ? 'bg-white text-zinc-900 shadow-sm dark:bg-white/15 dark:text-white'
              : 'text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300'
          }`}
        >
          {icon}
        </button>
      ))}
    </span>
  )
}

export default function Home() {
  const [entries, setEntries] = useState<ServerEntry[]>([])
  const [currency, setCurrency] = useState('CNY')
  const [summary, setSummary] = useState<Summary | null>(null)
  const [groups, setGroups] = useState<Group[]>([])
  const [expiring, setExpiring] = useState<{ items: ExpiringItem[]; currency: string } | null>(null)
  const [group, setGroup] = useState<number | 'all'>('all')
  // 状态筛选和分组筛选是**两个维度**，可以叠加：
  // 「亚太分组里哪几台离线了」是个真实问题，硬凑成一组 tab 就答不了
  const [status, setStatus] = useState<StatusFilter>('all')
  // 视图偏好存本地：换一次视图不该每次刷新都被打回默认
  const [view, setView] = useState<'card' | 'list'>(() => {
    try {
      return localStorage.getItem(VIEW_KEY) === 'list' ? 'list' : 'card'
    } catch {
      return 'card'
    }
  })
  const [error, setError] = useState<string | null>(null)

  // ── REST：静态信息与账单。低频，60 秒一次就够 ──
  useEffect(() => {
    let alive = true
    const load = async () => {
      try {
        const [s, sum, gs, exp] = await Promise.all([
          api.servers(),
          api.summary(),
          api.groups().catch(() => [] as Group[]),
          api.expiring(7).catch(() => null),
        ])
        if (!alive) return
        setEntries(s.servers)
        setCurrency(s.display_currency)
        setSummary(sum)
        setGroups(gs)
        setExpiring(exp ? { items: exp.items, currency: exp.display_currency } : null)
        setError(null)
      } catch (e) {
        if (alive) setError(e instanceof Error ? e.message : String(e))
      }
    }
    load()
    const t = setInterval(load, REFRESH_MS)
    return () => {
      alive = false
      clearInterval(t)
    }
  }, [])

  // ── WS：高频字段。断了也不影响页面，只是不再刷新 ──
  useEffect(() => {
    const live = new Live((tick: Tick) => {
      setEntries((prev) =>
        prev.map((e) => {
          const u = tick.servers[e.server.id]
          if (!u) return e
          return {
            ...e,
            server: {
              ...e.server,
              online: u.online,
              cpu_pct: u.cpu,
              uptime_s: u.uptime_s,
              mem: { ...e.server.mem, used: u.mem_used, pct: u.mem_pct },
              disk: { ...e.server.disk, pct: u.disk_pct },
              net: { ...e.server.net, rx_speed: u.net_in, tx_speed: u.net_out },
              latency:
                u.rtt !== null && u.loss !== null
                  ? { task_id: e.server.latency?.task_id ?? 0, rtt_ms: u.rtt, loss_pct: u.loss }
                  : e.server.latency,
            },
          }
        }),
      )
      if (tick.summary) {
        setSummary((s) =>
          s
            ? {
                ...s,
                servers: {
                  total: tick.summary!.total,
                  online: tick.summary!.online,
                  offline: tick.summary!.offline,
                },
                network: { in_speed: tick.summary!.in_speed, out_speed: tick.summary!.out_speed },
              }
            : s,
        )
      }
    })
    live.start()
    return () => live.stop()
  }, [])

  const inGroup = useMemo(
    () =>
      group === 'all'
        ? entries
        : entries.filter((e) =>
            // 「未分组」用 0 表示 —— null 不能当 tab 的 key
            group === 0 ? e.server.group_id === null : e.server.group_id === group,
          ),
    [entries, group],
  )
  const shown = useMemo(() => inGroup.filter((e) => matchStatus(e, status)), [inGroup, status])
  const ungrouped = entries.filter((e) => e.server.group_id === null).length
  // 计数针对**当前分组**算，不是全局 —— 切到「亚太」时「离线 2」就该是亚太里的 2 台
  const counts = useMemo(
    () => ({
      offline: inGroup.filter((e) => matchStatus(e, 'offline')).length,
      busy: inGroup.filter((e) => matchStatus(e, 'busy')).length,
      expiring: inGroup.filter((e) => matchStatus(e, 'expiring')).length,
    }),
    [inGroup],
  )

  if (error && entries.length === 0) {
    return (
      <p className="rounded-xl border border-red-500/30 bg-red-500/5 p-4 text-sm text-red-600 dark:text-red-400">
        无法连接面板：{error}
      </p>
    )
  }

  return (
    // `overflow-hidden` 让溢出的地球被裁在页面内，不会撑出横向滚动条。
    // 包住**整页**而不只是统计区 —— 球要能一直伸到第一排卡片底下去。
    <div className="relative space-y-4 overflow-hidden">
      {/* ── 顶部：统计 + 地球 ──
          地球**不装在框里**，而是绝对定位到右上角、向右下溢出，
          卡片半透明地压在它上面（`.panel` 带 backdrop-blur，球会透出来）。

          为什么不并排放两个格子：并排就得让两边一样高，而地球是正方形、
          统计块是两行卡片，高度天生对不上 —— 之前为这件事来回调了三轮
          （拉伸指标卡 → 撑成空盒子；顶部对齐 → 底下留一条；绝对定位 → 齐了但球只剩 170px）。
          让它溢出，这个约束就整个消失了，而且球能大到六百多像素。

          `overflow-hidden` 是必须的：不裁的话球伸出去会把整页撑出横向滚动条。 */}
      {/* 球在最底层。`pointer-events-none` 让它不挡住上面的卡片，
          SVG 自己再把交互打开 —— 于是没被卡片盖住的那部分仍然能拖能悬停 */}
      <Globe
        servers={entries}
        className="pointer-events-none mx-auto w-[min(12rem,50vw)]
                   lg:absolute lg:right-6 lg:top-4 lg:mx-0 lg:w-[18rem]
                   xl:right-10 xl:w-[21rem] [&_svg]:pointer-events-auto"
      />

      <div className="relative">
        {/* 统计只占左边一半多一点，右边留给球。
            到期提醒跟在下面、共用这个宽度 —— 它是汇总信息的一部分，
            铺满整个宽度的话名字在最左、金额在最右，中间一大片空 */}
        <div className="space-y-4 lg:w-[62%] xl:w-[58%]">
          <StatBar s={summary} servers={entries} />
          {expiring && <Expiring items={expiring.items} currency={expiring.currency} />}
        </div>

        {/* 图例浮在球上面。窄屏时跟在统计下面，宽屏时贴到右上 */}
        <GlobeLegend
          servers={entries}
          className="mt-3 justify-center lg:absolute lg:right-1 lg:top-0 lg:mt-0 lg:max-w-[34%] lg:justify-end"
        />
      </div>

      <div className="relative">
        <div className="space-y-4">
          {/* 分组（R11）+ 状态筛选。
              两个维度可以叠加：分组选「亚太」、状态选「离线」，
              答的是「亚太这几台里哪些掉了」—— 这个问题只有一排 tab 是答不了的。 */}
          <nav className="flex flex-wrap items-center gap-1.5" aria-label="筛选">
            {groups.length > 0 && (
              <>
                <GroupTab active={group === 'all'} onClick={() => setGroup('all')}>
                  全部 ({entries.length})
                </GroupTab>
                {groups.map((g) => {
                  const n = entries.filter((e) => e.server.group_id === g.id).length
                  return (
                    <GroupTab key={g.id} active={group === g.id} onClick={() => setGroup(g.id)}>
                      {g.icon} {g.name} ({n})
                    </GroupTab>
                  )
                })}
                {ungrouped > 0 && (
                  <GroupTab active={group === 0} onClick={() => setGroup(0)}>
                    未分组 ({ungrouped})
                  </GroupTab>
                )}
                <span className="mx-1 h-4 w-px bg-black/10 dark:bg-white/15" aria-hidden />
              </>
            )}

            {/* 状态筛选。两条规则：
                1. **计数为 0 的不显示**。「离线 0」「即将到期 0」是好消息，
                   不需要占位；真出问题时它自己会冒出来，反而更醒目
                2. **没有「全部」这一项**。左边分组那排已经有一个「全部」了，
                   两个挨着谁也分不清管的是什么。改成再点一次取消筛选 */}
            {counts.offline > 0 && (
              <GroupTab
                active={status === 'offline'}
                onClick={() => setStatus(status === 'offline' ? 'all' : 'offline')}
                tone="danger"
                title="只看离线的。再点一次取消"
              >
                离线 ({counts.offline})
              </GroupTab>
            )}
            {counts.busy > 0 && (
              <GroupTab
                active={status === 'busy'}
                onClick={() => setStatus(status === 'busy' ? 'all' : 'busy')}
                tone="warn"
                title="CPU 或内存超过 80%。再点一次取消"
              >
                高负载 ({counts.busy})
              </GroupTab>
            )}
            {counts.expiring > 0 && (
              <GroupTab
                active={status === 'expiring'}
                onClick={() => setStatus(status === 'expiring' ? 'all' : 'expiring')}
                tone="warn"
                title="7 天内到期或已过期。再点一次取消"
              >
                即将到期 ({counts.expiring})
              </GroupTab>
            )}

            <span className="ml-auto">
              <ViewToggle view={view} onChange={setView} />
            </span>
          </nav>

          {shown.length === 0 && (
            <p className="rounded-xl border border-dashed border-black/15 p-8 text-center text-sm text-zinc-500 dark:border-white/15">
              还没有机器。在后台添加一台，然后用生成的命令安装探针。
            </p>
          )}
          {shown.length > 0 && view === 'card' && (
            <div className="card-grid grid gap-3">
              {shown.map((e) => (
                <ServerCard key={e.server.id} e={e} currency={currency} />
              ))}
            </div>
          )}
          {shown.length > 0 && view === 'list' && (
            // 窄屏横向滚动而不是折行 —— 列表的价值就是同一列上下对齐
            <div className="panel overflow-x-auto p-3">
              <table className="w-full text-xs">
                <thead className="text-left text-zinc-500">
                  <tr>
                    {['名称', '状态', 'CPU', '内存', '硬盘', '网速', '流量', '价格', '剩余'].map((h) => (
                      <th key={h} className="whitespace-nowrap pb-1.5 pr-3 font-medium">
                        {h}
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {shown.map((e) => (
                    <ServerRow key={e.server.id} e={e} currency={currency} />
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}

/**
 * 状态筛选。和分组是两个维度，可以叠加。
 *
 * 阈值都写在这里而不是散在各处：
 * - **高负载**：CPU 或内存超 80%。只看在线的 —— 离线机器的旧数据不算「正在高负载」
 * - **即将到期**：7 天内（含已过期）。和后台「到期提醒」用同一个口径
 */
export type StatusFilter = 'all' | 'offline' | 'busy' | 'expiring'

const BUSY_PCT = 80
const EXPIRING_DAYS = 7

export function matchStatus(e: ServerEntry, f: StatusFilter): boolean {
  const s = e.server
  switch (f) {
    case 'all':
      return true
    case 'offline':
      return !s.online
    case 'busy':
      return s.online && (s.cpu_pct >= BUSY_PCT || (s.mem.total > 0 && s.mem.pct >= BUSY_PCT))
    case 'expiring': {
      const b = e.billing
      if (!b || b.infinite || b.unknown_expiry || b.remain_days == null) return false
      return b.remain_days <= EXPIRING_DAYS
    }
  }
}

function GroupTab({
  active,
  onClick,
  tone,
  title,
  children,
}: {
  active: boolean
  onClick: () => void
  /** 未选中时的文字颜色。离线用红、告警用琥珀 —— 不点开也能看出有问题 */
  tone?: 'danger' | 'warn'
  title?: string
  children: React.ReactNode
}) {
  const idle =
    tone === 'danger' ? 'text-red-600 hover:bg-red-500/10 dark:text-red-400'
    : tone === 'warn' ? 'text-amber-600 hover:bg-amber-500/10 dark:text-amber-500'
    : 'text-zinc-500 hover:bg-black/5 dark:hover:bg-white/10'
  const on =
    tone === 'danger' ? 'bg-red-500/15 text-red-700 dark:text-red-400'
    : tone === 'warn' ? 'bg-amber-500/15 text-amber-700 dark:text-amber-500'
    : 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-400'
  return (
    <button
      onClick={onClick}
      title={title}
      aria-current={active ? 'true' : undefined}
      aria-pressed={title ? active : undefined}
      className={`rounded-lg px-2.5 py-1 text-sm transition ${active ? on : idle}`}
    >
      {children}
    </button>
  )
}
