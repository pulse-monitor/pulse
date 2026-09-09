import { Link } from 'react-router-dom'
import type { ServerEntry } from '../api'
import * as f from '../lib/format'
import { Bar } from './Bar'
import { Flag } from './Flag'
import { IconCpu, IconDisk, IconLatency, IconLoss, IconMemory, IconTraffic } from './icons'
import { OsIcon } from './OsIcon'
import { Spark } from './Spark'

/**
 * 服务器卡片。对应 R16 的 17 个字段。
 *
 * 布局是 **2×2 的指标网格**：每格「图标 + 名称 + 右对齐百分比 / 进度条 /
 * 绝对值」。相比一行一个指标，同样的高度能多放一倍信息，
 * 而且百分比在同一列上下对齐，扫一眼就能比较。
 *
 * 核心规则不变：**`capabilities` 说采不到的字段整块隐藏，而不是显示 0**。
 * 显示 0 是撒谎 —— 用户会以为「温度 0°C」「进程数 0」是真的。
 */
export function ServerCard({ e, currency }: { e: ServerEntry; currency: string }) {
  const s = e.server
  const c = s.capabilities
  const t = e.traffic
  const b = e.billing
  // 从没上报过：显示「未安装探针」而不是「离线 0 分」和一堆 0
  const neverSeen = !s.online && s.last_seen === 0
  const live = s.online
  // 只列采得到的。采不到就整项不出现 —— 显示 0 是撒谎
  const details = !live
    ? []
    : [
        c.proc_count && s.proc_count != null ? `${s.proc_count} 进程` : null,
        c.tcp_conn_count && s.tcp_conn != null ? `${s.tcp_conn} 连接` : null,
        s.cpu_temp != null ? `${s.cpu_temp.toFixed(0)}°C` : null,
        s.gpu ? `GPU ${(s.gpu.util / 100).toFixed(0)}%` : null,
      ].filter((x): x is string => x !== null)

  return (
    // 卡片外层不能再是 <a>：里面还要放「购买同款 / 测评」两个外链，
    // <a> 套 <a> 是非法 HTML，浏览器会把结构拆开、键盘顺序也会乱。
    <div
      className="panel flex h-full flex-col p-3 transition
                 hover:border-emerald-400 hover:shadow-md focus-within:border-emerald-500
                 dark:hover:border-emerald-500/60"
    >
      {/* ── 标题行：状态点 / 名称（进详情） / 外链 / 国旗 ── */}
      <div className="flex items-center gap-1.5">
        <span
          className={`${s.online ? 'dot-on text-emerald-500' : 'dot-off text-zinc-400'} text-[10px]`}
          aria-label={s.online ? '在线' : '离线'}
        />
        <Link
          to={`/server/${s.id}`}
          className="truncate font-semibold hover:text-emerald-600 focus-visible:outline-2 focus-visible:outline-emerald-500"
        >
          {s.name}
        </Link>
        {/* 系统图标放在国旗前面：一眼能看出「哪个国家的什么系统」 */}
        <span className="ml-auto flex shrink-0 items-center gap-1.5">
          <OsIcon os={s.os} />
          <Flag code={s.country_code} className="text-base" />
        </span>
      </div>

      {/* ── 徽章行：在线时长 / 价格 ── */}
      <div className="mt-1.5 flex flex-wrap items-center gap-1">
        {neverSeen ? (
          <Badge tone="warn">未安装探针</Badge>
        ) : (
          <Badge>{live ? `在线 ${f.duration(s.uptime_s)}` : `离线 ${f.duration(nowMinus(s.last_seen))}`}</Badge>
        )}
        {/* 价格语义（-1 免费 / 0 不显示 / >0 金额）收在 `f.priceKind` 里，
            这里只管两种底色不同的 Badge 怎么画。 */}
        {f.priceKind(b) === 'free' && <Badge tone="ok">免费</Badge>}
        {f.priceKind(b) === 'amount' && b && (
          <Badge>
            {f.money(b.price_display, currency)}
            {b.cycle !== 'onetime' && ' / 周期'}
            {b.rate_level !== 'fresh' && (
              <sup className="ml-0.5 text-amber-500" title={rateHint(b.rate_level)}>
                *
              </sup>
            )}
          </Badge>
        )}
        {/* 虚拟化类型只在**认得出具体是什么**时才显示。
            `vm` 是「CPUID 说是虚拟机但认不出厂商」的兜底值，
            `none` 是物理机 —— 这两个挂在卡片上没有任何信息量。 */}
        {s.virtualization && !['none', 'vm', ''].includes(s.virtualization) && (
          <Badge>{s.virtualization}</Badge>
        )}
      </div>

      {/* 从没上报过的机器：**不画那一整套指标骨架**。
          四个进度条全是灰槽、六个数字全是 0 和 —— 占掉大半张卡片却一个字的
          信息量都没有，还让同一行里「有数据」和「没数据」的卡片长得一样，
          反倒要多看两眼才分得清。这里直接告诉用户下一步该干什么。

          外层的 `flex-1` 吃掉多余高度（同一行的卡片要等高），
          灰底的提示框本身保持正常大小 —— 让它跟着一起撑大会变成一个巨大的空盒子。 */}
      {neverSeen ? (
        <div className="mt-2.5 flex flex-1 items-center">
          <div className="w-full rounded-lg bg-black/[0.03] px-3 py-4 text-[11px] leading-relaxed text-zinc-500 dark:bg-white/[0.04]">
            还没收到过上报。到后台复制安装命令，在这台机器上执行即可。
          </div>
        </div>
      ) : (
        <>
      {/* ── 2×2 指标网格 ── */}
      <div className="mt-2.5 grid grid-cols-2 gap-x-4 gap-y-2 text-xs">
        <Metric
          icon={<IconCpu className="h-3.5 w-3.5 text-sky-500" />}
          label="CPU"
          pct={live ? s.cpu_pct : null}
          // CPU 下面放 1/5/15 负载：比再画一条进度条有用得多
          sub={
            c.load_average && s.load1 != null
              ? `${s.load1.toFixed(2)}, ${(s.load5 ?? 0).toFixed(2)}, ${(s.load15 ?? 0).toFixed(2)}`
              : s.cpu_cores > 0
                ? `${s.cpu_cores} 核`
                : ''
          }
        />
        <Metric
          icon={<IconMemory className="h-3.5 w-3.5 text-emerald-500" />}
          label="内存"
          pct={live && s.mem.total > 0 ? s.mem.pct : null}
          sub={s.mem.total > 0 ? `${f.bytes(s.mem.used)} / ${f.bytes(s.mem.total)}` : ''}
        />
        <Metric
          icon={<IconDisk className="h-3.5 w-3.5 text-amber-500" />}
          label="硬盘"
          pct={live && s.disk.total > 0 ? s.disk.pct : null}
          sub={s.disk.total > 0 ? `${f.bytes(s.disk.used)} / ${f.bytes(s.disk.total)}` : ''}
        />
        <Metric
          icon={<IconTraffic className="h-3.5 w-3.5 text-violet-500" />}
          label="流量"
          // 未设上限时显示 ♾️ 且不画进度条
          pct={t?.pct ?? null}
          pctText={t?.limit == null ? '∞' : undefined}
          sub={t ? `${f.bytes(t.used)} / ${f.limit(t.limit)}` : ''}
        />
      </div>

      {/* ── 实时网速 / 累计流量 / 剩余 ──
          **必须是固定三列的 grid，每格两行**，不能用 flex 一行铺开：
          网速的位数一变（`262 B/s` → `2.4 MiB/s`），flex 就会把「剩余 X 天」
          挤到下一行，卡片高度跟着跳。grid 的列宽由容器决定、与内容无关，
          数字再怎么变都不会重排。 */}
      <div className="mt-2 grid grid-cols-3 gap-x-2 border-t border-black/5 pt-2 text-[11px] tabular-nums dark:border-white/10">
        <div className="flex flex-col gap-0.5 truncate">
          <span className="truncate text-emerald-600 dark:text-emerald-400">
            ↑ {f.speed(live ? s.net.tx_speed : 0)}
          </span>
          <span className="truncate text-sky-600 dark:text-sky-400">
            ↓ {f.speed(live ? s.net.rx_speed : 0)}
          </span>
        </div>
        <div className="flex flex-col gap-0.5 truncate text-zinc-500">
          <span className="truncate">⇧ {f.bytes(t?.out_bytes, 1)}</span>
          <span className="truncate">⇩ {f.bytes(t?.in_bytes, 1)}</span>
        </div>
        {/* 完全没填过账单信息时整列留空 —— 一个孤零零的「—」吊在右下角，
            比什么都不显示更让人以为是出错了 */}
        <div className="flex flex-col items-end gap-0.5 truncate">
          {b && (
            <>
              <span className={`truncate ${remainTone(b)}`}>{remainText(b)}</span>
              <span className="truncate text-zinc-500">
                {b.auto_renew ? '自动续费' : '不自动续费'}
              </span>
            </>
          )}
        </div>
      </div>

      {/* ── 延迟与丢包 ──
          判据是「**有没有探测结果**」，不是 `capabilities.icmp_unprivileged` ——
          后者只说明内核允许非特权 ping，不代表后台真的配了监控地址。
          没配的话整块不出现，而不是显示两个 `—` 占位。 */}
      {s.latency && (
        <div className="mt-2 grid grid-cols-2 gap-2 text-[11px]">
          <Cell>
            <span className="flex items-baseline justify-between">
              <span className="flex items-baseline gap-1.5 text-zinc-500">
                <IconLatency className="h-3.5 w-3.5 translate-y-px text-sky-500" />
                延迟
              </span>
              <span className="tabular-nums">{s.latency.rtt_ms.toFixed(0)} ms</span>
            </span>
            <Spark values={s.latency_spark ?? []} tone="sky" />
          </Cell>
          <Cell>
            <span className="flex items-baseline justify-between">
              <span className="flex items-baseline gap-1.5 text-zinc-500">
                <IconLoss className="h-3.5 w-3.5 translate-y-px text-amber-500" />
                丢包
              </span>
              <span className="tabular-nums">{s.latency.loss_pct.toFixed(0)}%</span>
            </span>
            <Spark values={s.loss_spark ?? []} tone="amber" />
          </Cell>
        </div>
      )}

        </>
      )}

      {/* ── 底部：采得到的细节 + 购买 / 测评外链 ──
          外链放在最下面而不是标题旁：它们是「关于这台机器的资料」，
          不是运行状态，混在徽章行里会跟在线时长、价格抢注意力。 */}
      {(details.length > 0 || s.buy_url || s.review_url) && (
        <div className="mt-auto flex flex-wrap items-center gap-x-3 gap-y-1 pt-1.5 text-[11px] text-zinc-500">
          {details.map((d) => (
            <span key={d}>{d}</span>
          ))}
          {(s.buy_url || s.review_url) && (
            <span className="ml-auto flex items-center gap-1.5">
              {s.buy_url && (
                <ExtLink href={s.buy_url} title="购买同款机器">
                  🛒 购买同款
                </ExtLink>
              )}
              {s.review_url && (
                <ExtLink href={s.review_url} title="查看测评">
                  📝 测评
                </ExtLink>
              )}
            </span>
          )}
        </div>
      )}
    </div>
  )
}

/** 卡片上的外链小按钮。`noopener` 是必须的：目标站能通过 `window.opener` 改我们这一页。 */
function ExtLink({ href, title, children }: { href: string; title: string; children: React.ReactNode }) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noreferrer noopener"
      title={title}
      className="rounded bg-sky-500/10 px-1.5 py-0.5 text-[11px] text-sky-700 transition
                 hover:bg-sky-500/20 focus-visible:outline-2 focus-visible:outline-emerald-500
                 dark:text-sky-400"
    >
      {children}
    </a>
  )
}

function Badge({ children, tone }: { children: React.ReactNode; tone?: 'warn' | 'ok' }) {
  const cls = {
    warn: 'bg-amber-500/15 text-amber-700 dark:text-amber-400',
    ok: 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-400',
    undefined: 'bg-black/5 text-zinc-600 dark:bg-white/10 dark:text-zinc-400',
  }[String(tone)]
  return <span className={`rounded px-1.5 py-0.5 text-[11px] ${cls}`}>{children}</span>
}

function Cell({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-0.5 rounded-lg bg-black/[0.03] px-2 py-1.5 tabular-nums dark:bg-white/[0.04]">
      {children}
    </div>
  )
}

/**
 * 一格指标：标签 + 右对齐百分比 / 进度条 / 绝对值。
 *
 * `pct` 为 null 时进度条画灰槽而不是 0% 的绿条 —— 「不知道」和「0%」是两回事。
 */
function Metric({
  icon,
  label,
  pct,
  pctText,
  sub,
}: {
  icon: React.ReactNode
  label: string
  pct: number | null
  pctText?: string
  sub?: string
}) {
  return (
    <div className="min-w-0">
      <div className="flex items-baseline justify-between gap-1">
        {/* 图标按指标分色（CPU 蓝、内存绿、硬盘橙、流量紫）。
            不是为了好看 —— 2×2 网格里四项长得一样，扫的时候得先读字才知道
            自己看的是哪一项；有颜色就能直接定位。
            `translate-y-px` 是因为图标按盒子居中，而旁边的文字按基线对齐 */}
        <span className="flex min-w-0 items-baseline gap-1.5 text-zinc-500">
          <span className="translate-y-px">{icon}</span>
          {label}
        </span>
        <span className="tabular-nums">{pctText ?? (pct == null ? '—' : f.pct(pct))}</span>
      </div>
      <Bar pct={pct} className="my-1" />
      {/* 没有副值时留一个不换行空格：两列的副行高度必须一样，
          否则一边有「0 B / 3.8 GiB」另一边空着，进度条就错位了 */}
      <div className="truncate text-[11px] tabular-nums text-zinc-500">{sub || '\u00a0'}</div>
    </div>
  )
}

function remainText(b: ServerEntry['billing']): string {
  if (!b) return '—'
  if (b.infinite) return '长期'
  if (b.unknown_expiry) return '未设到期'
  if (b.remain_days == null) return '—'
  return b.remain_days < 0 ? `已过期 ${-b.remain_days} 天` : `剩余 ${b.remain_days} 天`
}

function remainTone(b: ServerEntry['billing']): string {
  if (!b || b.infinite || b.unknown_expiry || b.remain_days == null) return 'text-zinc-500'
  if (b.remain_days < 0) return 'text-red-500'
  if (b.remain_days <= 7) return 'text-amber-600 dark:text-amber-400'
  return ''
}

function rateHint(level: string): string {
  return level === 'missing'
    ? '这个币种没有汇率，未计入总价值'
    : level === 'manual'
      ? '用的是人工填的汇率'
      : '汇率数据已过期，用的是最后一次成功的值'
}

/** 距今多久（秒）。`ts` 为 0 表示从没上报过，返回 0 让调用方另行处理。 */
function nowMinus(ts: number): number {
  return ts > 0 ? Math.max(0, Math.floor(Date.now() / 1000) - ts) : 0
}
