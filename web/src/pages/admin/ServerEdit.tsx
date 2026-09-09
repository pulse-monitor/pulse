import { useEffect, useState } from 'react'
import { get, post, put } from '../../api'
import { Btn, Card, Check, Err, Field, Num, Select, Text, fromDate, run, toDate } from './ui'
import { CALC_MODES, CURRENCIES, CYCLES } from './consts'

interface Group {
  id: number
  name: string
}

/** 套餐：同款机器共用的价格与购买 / 测评链接模板。 */
interface Plan {
  id: number
  name: string
  provider: string | null
  buy_url: string | null
  review_url: string | null
  price: number | null
  currency: string | null
  cycle: string | null
}

interface Billing {
  price: number
  currency: string
  cycle: string
  custom_cycle_days: number | null
  cycle_start_at: number | null
  expire_at: number | null
  auto_renew: boolean
  purchased_at: number | null
  remark: string | null
}

interface TrafficConf {
  limit_bytes: number | null
  calc_mode: string
  reset_day: number
  timezone: string | null
  alert_pct: number[]
}

interface RuntimeConf {
  interval_s: number
  net_include: string[]
  net_exclude: string[]
  disk_include: string[]
  disk_exclude: string[]
  gpu_enabled: boolean
  report_temps: boolean
  report_conn_count: boolean
}

const GiB = 1024 ** 3
const listToStr = (a: string[] | null | undefined) => (a ?? []).join(', ')
const strToList = (s: string) =>
  s
    .split(/[,\s]+/)
    .map((x) => x.trim())
    .filter(Boolean)

/**
 * 单台机器的全部可编辑项：基本信息 / 位置 / 账单(R4) / 流量(R5) / 采集(R3)。
 *
 * 每个分区**独立保存**，各打各的接口 —— 一个大表单一次提交的话，
 * 改个备注就得把账单和流量配置一起重发，任何一处校验失败全部作废。
 */
export default function ServerEdit({
  id,
  name,
  groups,
  onSaved,
}: {
  id: number
  name: string
  groups: Group[]
  onSaved: () => void
}) {
  const [err, setErr] = useState<string | null>(null)
  const [ok, setOk] = useState<string | null>(null)
  const done = (what: string) => {
    setOk(`${what}已保存`)
    setErr(null)
    onSaved()
  }

  return (
    <div className="mt-2 space-y-3 border-t border-black/10 pt-3 dark:border-white/10">
      <Err msg={err} />
      {ok && <p className="text-sm text-emerald-600">{ok}</p>}
      <Basic id={id} name={name} groups={groups} setErr={setErr} done={done} />
      <BillingForm id={id} setErr={setErr} done={done} />
      <TrafficForm id={id} setErr={setErr} done={done} />
      <RuntimeForm id={id} setErr={setErr} done={done} />
    </div>
  )
}

type Sub = { id: number; setErr: (m: string | null) => void; done: (w: string) => void }

function Basic({ id, name, groups, setErr, done }: Sub & { name: string; groups: Group[] }) {
  const [v, setV] = useState({
    name,
    group_id: '' as string,
    country_code: '',
    region: '',
    latitude: null as number | null,
    longitude: null as number | null,
    buy_url: '',
    review_url: '',
    note: '',
    hidden: false,
  })
  const [loaded, setLoaded] = useState(false)

  useEffect(() => {
    get<Record<string, unknown>>(`/api/v1/admin/servers/${id}`)
      .then((s) =>
        setV({
          name: (s.name as string) ?? name,
          group_id: s.group_id == null ? '' : String(s.group_id),
          country_code: (s.country_code as string) ?? '',
          region: (s.region as string) ?? '',
          latitude: (s.latitude as number) ?? null,
          longitude: (s.longitude as number) ?? null,
          buy_url: (s.buy_url as string) ?? '',
          review_url: (s.review_url as string) ?? '',
          note: (s.note as string) ?? '',
          hidden: Boolean(s.hidden),
        }),
      )
      .catch((e) => setErr(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoaded(true))
  }, [id])

  const set = (p: Partial<typeof v>) => setV({ ...v, ...p })

  return (
    <Card title="基本信息与位置">
      <div className="grid gap-2 sm:grid-cols-3">
        <Field label="名称">
          <Text value={v.name} onChange={(x) => set({ name: x })} disabled={!loaded} />
        </Field>
        <Field label="分组">
          <Select
            value={v.group_id}
            onChange={(x) => set({ group_id: x })}
            options={[['', '未分组'], ...groups.map((g) => [String(g.id), g.name] as [string, string])]}
          />
        </Field>
        <Field label="国家代码" hint="两位 ISO 代码，决定卡片上的国旗">
          <Text
            value={v.country_code}
            onChange={(x) => set({ country_code: x.toUpperCase() })}
            maxLength={2}
            placeholder="JP"
          />
        </Field>
        <Field label="地区" hint="自由文本，如 Tokyo">
          <Text value={v.region} onChange={(x) => set({ region: x })} />
        </Field>
        <Field label="纬度" hint="留空则用国家中心点标在地球上">
          <Num value={v.latitude} onChange={(x) => set({ latitude: x })} step="0.0001" />
        </Field>
        <Field label="经度">
          <Num value={v.longitude} onChange={(x) => set({ longitude: x })} step="0.0001" />
        </Field>
        <Field label="购买同款链接">
          <Text value={v.buy_url} onChange={(x) => set({ buy_url: x })} placeholder="https://…" />
        </Field>
        <Field label="测评链接">
          <Text value={v.review_url} onChange={(x) => set({ review_url: x })} placeholder="https://…" />
        </Field>
        <Field label="备注">
          <Text value={v.note} onChange={(x) => set({ note: x })} />
        </Field>
      </div>
      <div className="mt-2 flex items-center gap-3">
        <Check
          checked={v.hidden}
          onChange={(x) => set({ hidden: x })}
          label="在公开页隐藏这台机器"
        />
        <Btn
          kind="primary"
          onClick={() =>
            run(
              () =>
                put(`/api/v1/admin/servers/${id}`, {
                  name: v.name.trim(),
                  group_id: v.group_id === '' ? null : Number(v.group_id),
                  country_code: v.country_code.trim() || null,
                  region: v.region.trim() || null,
                  latitude: v.latitude,
                  longitude: v.longitude,
                  // 手填了坐标就锁定，别被将来的自动定位覆盖
                  location_manual: v.latitude != null && v.longitude != null,
                  buy_url: v.buy_url.trim() || null,
                  review_url: v.review_url.trim() || null,
                  note: v.note.trim() || null,
                  hidden: v.hidden,
                }),
              () => done('基本信息'),
              (m) => setErr(m || null),
            )
          }
        >
          保存基本信息
        </Btn>
      </div>
    </Card>
  )
}

function BillingForm({ id, setErr, done }: Sub) {
  // 套餐不是独立的东西，它就是「这台机器的价格与链接从哪儿抄」。
  // 所以不给它单独一个后台菜单，直接放在账单表单里 —— 选一个就把
  // 价格、货币、周期、购买链接、测评链接一次填好，不用逐台手打。
  const [plans, setPlans] = useState<Plan[]>([])
  useEffect(() => {
    get<Plan[]>('/api/v1/admin/plans')
      .then(setPlans)
      .catch(() => {
        /* 没有套餐是正常状态，不是错误 */
      })
  }, [])
  const [b, setB] = useState<Billing>({
    price: 0,
    currency: 'USD',
    cycle: 'monthly',
    custom_cycle_days: null,
    cycle_start_at: null,
    expire_at: null,
    auto_renew: false,
    purchased_at: null,
    remark: null,
  })
  const [applied, setApplied] = useState<Plan | null>(null)
  useEffect(() => {
    // 注意这个接口返回的是 `{billing, remaining, renew_state}` 包装层，
    // 不是裸的账单对象 —— 整个包装塞回 PUT 会 422。
    get<{ billing: Billing | null }>(`/api/v1/admin/servers/${id}/billing`)
      .then((x) => x.billing && setB(x.billing))
      .catch(() => {
        /* 没设过账单时后端返回 404/空，这是正常状态，不是错误 */
      })
  }, [id])
  const set = (p: Partial<Billing>) => setB({ ...b, ...p })

  return (
    <Card title="账单">
      {plans.length > 0 && (
        <div className="mb-2 flex flex-wrap items-center gap-2 rounded-lg bg-black/[0.03] px-2 py-1.5 text-xs dark:bg-white/[0.04]">
          <span className="text-zinc-500">套用套餐</span>
          <select
            defaultValue=""
            onChange={(ev) => {
              const p = plans.find((x) => String(x.id) === ev.target.value)
              if (!p) return
              set({
                price: p.price ?? b.price,
                currency: p.currency ?? b.currency,
                cycle: p.cycle ?? b.cycle,
              })
              // 购买 / 测评链接属于「基本信息」，不在这张表单里 ——
              // 提示用户去上面那块保存，别让他以为已经填好了
              setApplied(p)
              ev.target.value = ''
            }}
            className="rounded border border-black/15 bg-transparent px-1 py-0.5 dark:border-white/15"
          >
            <option value="">选一个…</option>
            {plans.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
                {p.price != null ? ` · ${p.price} ${p.currency}/${p.cycle}` : ''}
              </option>
            ))}
          </select>
          {applied && (
            <span className="text-amber-600 dark:text-amber-500">
              已带入「{applied.name}」的价格，记得点下面的「保存账单」
              {(applied.buy_url || applied.review_url) &&
                '；购买 / 测评链接请到上面的「基本信息」里填'}
            </span>
          )}
        </div>
      )}
      <div className="grid gap-2 sm:grid-cols-3">
        <Field label="价格" hint="填 -1 表示免费机器；填 0 表示不在前端显示价格">
          <Num value={b.price} onChange={(v) => set({ price: v ?? 0 })} step="0.01" min="-1" />
        </Field>
        <Field label="货币" hint="不在自动汇率覆盖内的需在「汇率」页手工填">
          <Select value={b.currency} onChange={(v) => set({ currency: v })} options={CURRENCIES} />
        </Field>
        <Field label="计费周期">
          <Select value={b.cycle} onChange={(v) => set({ cycle: v })} options={CYCLES} />
        </Field>
        {b.cycle === 'custom' && (
          <Field label="自定义周期天数">
            <Num value={b.custom_cycle_days} onChange={(v) => set({ custom_cycle_days: v })} min="1" />
          </Field>
        )}
        <Field label="购买日期">
          <input
            type="date"
            className="w-full rounded-lg border border-black/15 bg-transparent px-2 py-1.5 text-sm dark:border-white/15"
            value={toDate(b.purchased_at)}
            onChange={(e) => set({ purchased_at: fromDate(e.target.value) })}
          />
        </Field>
        <Field label="本周期起始">
          <input
            type="date"
            className="w-full rounded-lg border border-black/15 bg-transparent px-2 py-1.5 text-sm dark:border-white/15"
            value={toDate(b.cycle_start_at)}
            onChange={(e) => set({ cycle_start_at: fromDate(e.target.value) })}
          />
        </Field>
        <Field label="到期时间" hint="决定「剩余时间」与「剩余价值」">
          <input
            type="date"
            className="w-full rounded-lg border border-black/15 bg-transparent px-2 py-1.5 text-sm dark:border-white/15"
            value={toDate(b.expire_at)}
            onChange={(e) => set({ expire_at: fromDate(e.target.value) })}
          />
        </Field>
        <Field label="备注">
          <Text value={b.remark ?? ''} onChange={(v) => set({ remark: v })} />
        </Field>
      </div>
      <div className="mt-2 flex items-center gap-3">
        <Check checked={b.auto_renew} onChange={(v) => set({ auto_renew: v })} label="自动续费" />
        <Btn
          kind="primary"
          onClick={() =>
            run(
              () =>
                put(`/api/v1/admin/servers/${id}/billing`, {
                  ...b,
                  remark: b.remark?.trim() || null,
                  custom_cycle_days: b.cycle === 'custom' ? b.custom_cycle_days : null,
                }),
              () => done('账单'),
              (m) => setErr(m || null),
            )
          }
        >
          保存账单
        </Btn>
      </div>
    </Card>
  )
}

function TrafficForm({ id, setErr, done }: Sub) {
  const [t, setT] = useState<TrafficConf>({
    limit_bytes: null,
    calc_mode: 'sum',
    reset_day: 1,
    timezone: null,
    alert_pct: [80, 95],
  })
  useEffect(() => {
    get<TrafficConf | null>(`/api/v1/admin/servers/${id}/traffic-config`)
      .then((x) => x && setT({ ...x, alert_pct: x.alert_pct ?? [] }))
      .catch(() => {
        /* 未配置时 404，属正常 */
      })
  }, [id])
  const set = (p: Partial<TrafficConf>) => setT({ ...t, ...p })

  return (
    <Card title="流量">
      <div className="grid gap-2 sm:grid-cols-3">
        <Field label="月流量上限 (GiB)" hint="留空 = 不限，卡片上显示 ♾️">
          <Num
            value={t.limit_bytes == null ? null : Math.round(t.limit_bytes / GiB)}
            onChange={(v) => set({ limit_bytes: v == null ? null : Math.round(v * GiB) })}
            min="0"
          />
        </Field>
        <Field label="统计方式">
          <Select value={t.calc_mode} onChange={(v) => set({ calc_mode: v })} options={CALC_MODES} />
        </Field>
        <Field label="月重置日" hint="1–31；当月没有该日时按月末算">
          <Num value={t.reset_day} onChange={(v) => set({ reset_day: v ?? 1 })} min="1" max="31" />
        </Field>
        <Field label="时区" hint="留空用面板时区，如 Asia/Shanghai">
          <Text value={t.timezone ?? ''} onChange={(v) => set({ timezone: v })} />
        </Field>
        <Field label="告警阈值 (%)" hint="逗号分隔，如 80, 95">
          <Text
            value={t.alert_pct.join(', ')}
            onChange={(v) =>
              set({
                alert_pct: strToList(v)
                  .map(Number)
                  .filter((n) => Number.isFinite(n) && n > 0 && n <= 100),
              })
            }
          />
        </Field>
      </div>
      <div className="mt-2 flex flex-wrap items-center gap-2">
        <Btn
          kind="primary"
          onClick={() =>
            run(
              () =>
                put(`/api/v1/admin/servers/${id}/traffic-config`, {
                  ...t,
                  timezone: t.timezone?.trim() || null,
                }),
              () => done('流量配置'),
              (m) => setErr(m || null),
            )
          }
        >
          保存流量配置
        </Btn>
        <Btn
          kind="danger"
          onClick={() =>
            confirm('把这台机器本计费周期的流量清零？历史数据保留，只重置计数起点。') &&
            run(
              () => post(`/api/v1/admin/servers/${id}/reset-traffic`, {}),
              () => done('流量已重置，'),
              (m) => setErr(m || null),
            )
          }
        >
          重置本期流量
        </Btn>
      </div>
    </Card>
  )
}

function RuntimeForm({ id, setErr, done }: Sub) {
  const [c, setC] = useState<RuntimeConf>({
    interval_s: 3,
    net_include: [],
    net_exclude: [],
    disk_include: [],
    disk_exclude: [],
    gpu_enabled: false,
    report_temps: true,
    report_conn_count: true,
  })
  useEffect(() => {
    get<RuntimeConf>(`/api/v1/admin/servers/${id}/runtime-config`)
      .then(setC)
      .catch((e) => setErr(e instanceof Error ? e.message : String(e)))
  }, [id])
  const set = (p: Partial<RuntimeConf>) => setC({ ...c, ...p })

  return (
    <Card title="采集配置">
      <div className="grid gap-2 sm:grid-cols-3">
        <Field label="采集间隔（秒）" hint="1–60，agent 侧会再 clamp 一次">
          <Num value={c.interval_s} onChange={(v) => set({ interval_s: v ?? 3 })} min="1" max="60" />
        </Field>
        <Field label="只统计这些网卡" hint="glob，逗号分隔；留空 = 不限制">
          <Text value={listToStr(c.net_include)} onChange={(v) => set({ net_include: strToList(v) })} placeholder="eth0, ens*" />
        </Field>
        <Field label="排除这些网卡" hint="glob，逗号分隔">
          <Text value={listToStr(c.net_exclude)} onChange={(v) => set({ net_exclude: strToList(v) })} placeholder="docker*, veth*" />
        </Field>
        <Field label="只统计这些挂载点" hint="留空 = 自动（只算真实块设备）">
          <Text value={listToStr(c.disk_include)} onChange={(v) => set({ disk_include: strToList(v) })} placeholder="/, /data" />
        </Field>
        <Field label="排除这些挂载点">
          <Text value={listToStr(c.disk_exclude)} onChange={(v) => set({ disk_exclude: strToList(v) })} placeholder="/boot" />
        </Field>
      </div>
      <div className="mt-2 flex flex-wrap items-center gap-4">
        <Check checked={c.gpu_enabled} onChange={(v) => set({ gpu_enabled: v })} label="启用 GPU 监控" />
        <Check checked={c.report_temps} onChange={(v) => set({ report_temps: v })} label="上报温度" />
        <Check
          checked={c.report_conn_count}
          onChange={(v) => set({ report_conn_count: v })}
          label="上报连接数"
        />
        <Btn
          kind="primary"
          onClick={() =>
            run(
              () => put(`/api/v1/admin/servers/${id}/runtime-config`, c),
              () => done('采集配置'),
              (m) => setErr(m || null),
            )
          }
        >
          保存采集配置
        </Btn>
      </div>
      <p className="mt-2 text-[11px] text-zinc-400">
        改动会在 agent 下次连接或下一个心跳时下发，不需要重装。机器采不到的项（如没有
        NVML 时的 GPU）即使打开也不会有数据，卡片上会隐藏而不是显示 0。
      </p>
    </Card>
  )
}
