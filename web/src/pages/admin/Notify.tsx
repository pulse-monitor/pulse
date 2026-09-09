import { useState } from 'react'
import { del, get, post, put } from '../../api'
import { Btn, Card, Check, Err, Field, Num, Select, Text, run, useList } from './ui'
import { fromScope, ScopePicker, scopeText, toScope, type Scope } from './ScopePicker'
import { CHANNEL_KINDS } from './consts'

interface Channel {
  id: number
  name: string
  kind: string
  /** 后端回显的是**打码后**的配置，永远不会把明文凭据发回浏览器 */
  config: Record<string, unknown>
  enabled: boolean
}

/** `/admin/event-kinds` 的一项。`param` 非空表示这类事件带一个可调阈值。 */
interface EventKind {
  kind: string
  label: string
  param: string | null
  default: number | null
  /** 比较方向。`gte` = 超过阈值触发，`lte` = 低于阈值触发（如「剩余天数 ≤ 7」） */
  cmp: 'gte' | 'lte' | null
  unit: string
}

interface Rule {
  id: number
  name: string
  event_kinds: string[]
  channel_ids: number[]
  scope_kind: string | null
  scope_ids: number[] | null
  params: Record<string, number>
  duration_s: number
  cooldown_s: number
  notify_resolve: boolean
  enabled: boolean
}

/** 每种渠道要填的字段。和 `notify/{email,telegram,wecom,lark}.rs::Config` 一一对应。 */
const FIELDS: Record<string, { key: string; label: string; secret?: boolean; hint?: string }[]> = {
  telegram: [
    { key: 'token', label: 'Bot Token', secret: true },
    { key: 'chat_id', label: 'Chat ID' },
    { key: 'proxy', label: '代理', hint: '可留空，如 socks5://127.0.0.1:1080' },
  ],
  email: [
    { key: 'host', label: 'SMTP 主机' },
    { key: 'port', label: '端口' },
    { key: 'username', label: '用户名' },
    { key: 'password', label: '密码', secret: true },
    { key: 'from', label: '发件人' },
    { key: 'to', label: '收件人', hint: '多个用逗号分隔' },
  ],
  wecom: [{ key: 'key', label: 'Webhook Key', secret: true }],
  lark: [
    { key: 'webhook', label: 'Webhook 地址', secret: true },
    { key: 'secret', label: '签名密钥', hint: '开启签名校验时才需要' },
  ],
}

/** 通知渠道与规则（R7）。 */
export default function Notify() {
  const chans = useList(() => get<Channel[]>('/api/v1/admin/notification-channels'))
  const rules = useList(() => get<Rule[]>('/api/v1/admin/notification-rules'))
  const kinds = useList(() => get<EventKind[]>('/api/v1/admin/event-kinds'))
  // 作用范围要选分组或机器，得先知道有哪些
  const groups = useList(() => get<{ id: number; name: string }[]>('/api/v1/admin/groups'))
  const servers = useList(() => get<{ id: number; name: string }[]>('/api/v1/admin/servers'))
  const [editRule, setEditRule] = useState<Rule | null>(null)
  const [kind, setKind] = useState('telegram')
  const [name, setName] = useState('')
  const [cfg, setCfg] = useState<Record<string, string>>({})
  const [editing, setEditing] = useState<number | null>(null)
  const [tested, setTested] = useState<string | null>(null)

  const saveChannel = () => {
    if (!name.trim()) return chans.setErr('渠道名不能为空')
    const config: Record<string, unknown> = { kind }
    for (const f of FIELDS[kind] ?? []) {
      const v = cfg[f.key]?.trim()
      // 空字段不提交：编辑时这表示「不改这个凭据」，回显的是打码值
      if (v) config[f.key] = f.key === 'port' ? Number(v) : v
    }
    const body = { name: name.trim(), config, enabled: true }
    run(
      () =>
        editing
          ? put(`/api/v1/admin/notification-channels/${editing}`, body)
          : post('/api/v1/admin/notification-channels', body),
      () => {
        setEditing(null)
        setName('')
        setCfg({})
        chans.reload()
      },
      (m) => chans.setErr(m || null),
    )
  }

  const test = async (id: number) => {
    setTested(null)
    try {
      await post(`/api/v1/admin/notification-channels/${id}/test`, {})
      setTested(`渠道 #${id} 测试消息已发出，请到对应客户端确认收到`)
    } catch (e) {
      chans.setErr(e instanceof Error ? e.message : String(e))
    }
  }

  return (
    <div className="space-y-4">
      <Err msg={chans.err ?? rules.err} />
      {tested && <p className="text-sm text-emerald-600">{tested}</p>}

      <Card title={editing ? `编辑渠道 #${editing}` : '新建通知渠道'}>
        <div className="grid gap-2 sm:grid-cols-3">
          <Field label="渠道名">
            <Text value={name} onChange={setName} placeholder="我的 TG" />
          </Field>
          <Field label="类型">
            <Select
              value={kind}
              onChange={(v) => {
                setKind(v)
                setCfg({})
              }}
              options={CHANNEL_KINDS}
            />
          </Field>
          {(FIELDS[kind] ?? []).map((f) => (
            <Field key={f.key} label={f.label} hint={f.hint}>
              <Text
                value={cfg[f.key] ?? ''}
                onChange={(v) => setCfg({ ...cfg, [f.key]: v })}
                type={f.secret ? 'password' : 'text'}
                autoComplete="off"
              />
            </Field>
          ))}
        </div>
        <div className="mt-2 flex gap-2">
          <Btn kind="primary" onClick={saveChannel}>
            {editing ? '保存' : '添加'}
          </Btn>
          {editing && (
            <Btn
              onClick={() => {
                setEditing(null)
                setName('')
                setCfg({})
              }}
            >
              取消
            </Btn>
          )}
        </div>
        <p className="mt-2 text-[11px] text-zinc-400">
          凭据加密后落库，回显时一律打码。编辑时凭据字段留空 = 保持原值不变。
        </p>
      </Card>

      <ul className="space-y-1.5">
        {(chans.data ?? []).map((c) => (
          <li
            key={c.id}
            className="flex flex-wrap items-center gap-2 rounded-lg border border-black/10 bg-white px-3 py-2 text-sm dark:border-white/10 dark:bg-white/5"
          >
            <span className="font-medium">{c.name}</span>
            <span className="text-xs text-zinc-500">{c.kind}</span>
            {!c.enabled && <span className="text-xs text-amber-600">已停用</span>}
            <span className="ml-auto flex gap-1">
              <Btn onClick={() => test(c.id)}>发测试</Btn>
              <Btn
                onClick={() => {
                  setEditing(c.id)
                  setName(c.name)
                  setKind(c.kind)
                  setCfg({})
                }}
              >
                编辑
              </Btn>
              <Btn
                kind="danger"
                onClick={() =>
                  confirm(`删除渠道「${c.name}」？引用它的规则会失去这个发送目标。`) &&
                  run(
                    () => del(`/api/v1/admin/notification-channels/${c.id}`),
                    chans.reload,
                    (m) => chans.setErr(m || null),
                  )
                }
              >
                删除
              </Btn>
            </span>
          </li>
        ))}
        {chans.data?.length === 0 && <li className="text-sm text-zinc-500">还没有通知渠道</li>}
      </ul>

      <RuleForm
        key={editRule?.id ?? 'new'}
        kinds={kinds.data ?? []}
        channels={chans.data ?? []}
        groups={groups.data ?? []}
        servers={servers.data ?? []}
        editing={editRule}
        onCancel={() => setEditRule(null)}
        onSaved={() => {
          setEditRule(null)
          rules.reload()
        }}
      />

      <ul className="space-y-1.5">
        {(rules.data ?? []).map((r) => (
          <li
            key={r.id}
            className="flex flex-wrap items-center gap-2 rounded-lg border border-black/10 bg-white px-3 py-2 text-sm dark:border-white/10 dark:bg-white/5"
          >
            <span className="font-medium">{r.name}</span>
            <span className="text-xs text-zinc-500">
              {r.event_kinds
                .map((k) => (kinds.data ?? []).find((x) => x.kind === k)?.label ?? k)
                .join('、')}
              {' → '}
              {r.channel_ids
                .map((c) => (chans.data ?? []).find((x) => x.id === c)?.name ?? `#${c}`)
                .join('、') || '（未选渠道）'}
            </span>
            <span className="text-xs text-zinc-500">
              {scopeText(r.scope_kind, r.scope_ids, groups.data ?? [], servers.data ?? [])}
              {' · '}持续 {r.duration_s}s · 冷却 {r.cooldown_s}s
              {r.notify_resolve ? ' · 恢复也通知' : ''}
            </span>
            <span className="ml-auto flex gap-1">
              <Btn onClick={() => setEditRule(r)}>编辑</Btn>
              <Btn
                kind="danger"
                onClick={() =>
                  confirm(`删除规则「${r.name}」？`) &&
                  run(
                    () => del(`/api/v1/admin/notification-rules/${r.id}`),
                    rules.reload,
                    (m) => rules.setErr(m || null),
                  )
                }
              >
                删除
              </Btn>
            </span>
          </li>
        ))}
        {rules.data?.length === 0 && <li className="text-sm text-zinc-500">还没有通知规则</li>}
      </ul>
    </div>
  )
}

function RuleForm({
  kinds,
  channels,
  groups,
  servers,
  editing,
  onCancel,
  onSaved,
}: {
  kinds: EventKind[]
  channels: Channel[]
  groups: { id: number; name: string }[]
  servers: { id: number; name: string }[]
  editing: Rule | null
  onCancel: () => void
  onSaved: () => void
}) {
  const [name, setName] = useState(editing?.name ?? '')
  const [picked, setPicked] = useState<string[]>(editing?.event_kinds ?? [])
  const [params, setParams] = useState<Record<string, number>>(editing?.params ?? {})
  const [chan, setChan] = useState<number[]>(editing?.channel_ids ?? [])
  const [scope, setScope] = useState<Scope>(toScope(editing?.scope_kind ?? null, editing?.scope_ids ?? null))
  const [duration, setDuration] = useState(editing?.duration_s ?? 180)
  const [cooldown, setCooldown] = useState(editing?.cooldown_s ?? 3600)
  const [resolve, setResolve] = useState(editing?.notify_resolve ?? true)
  const [err, setErr] = useState<string | null>(null)

  const toggle = <T,>(arr: T[], v: T, set: (a: T[]) => void) =>
    set(arr.includes(v) ? arr.filter((x) => x !== v) : [...arr, v])

  return (
    <Card title={editing ? `编辑规则「${editing.name}」` : '新建通知规则'}>
      <Err msg={err} />
      <div className="grid gap-2 sm:grid-cols-3">
        <Field label="规则名">
          <Text value={name} onChange={setName} placeholder="离线告警" />
        </Field>
        <Field label="持续（秒）" hint="条件连续满足这么久才通知，避免抖动刷屏">
          <Num value={duration} onChange={(v) => setDuration(v ?? 0)} min="0" />
        </Field>
        <Field label="冷却（秒）" hint="同一告警两次通知的最小间隔">
          <Num value={cooldown} onChange={(v) => setCooldown(v ?? 0)} min="0" />
        </Field>
      </div>
      <fieldset className="mt-2">
        <legend className="mb-1 text-xs text-zinc-500">触发事件</legend>
        <div className="space-y-1">
          {kinds.map((k) => (
            <div key={k.kind} className="flex items-center gap-2">
              <Check
                checked={picked.includes(k.kind)}
                onChange={() => {
                  toggle(picked, k.kind, setPicked)
                  // 勾上带阈值的事件时，先填入后端给的默认值，用户可再改
                  if (k.param && !(k.param in params) && k.default != null) {
                    setParams((q) => ({ ...q, [k.param as string]: k.default as number }))
                  }
                }}
                label={k.label}
              />
              {k.param && picked.includes(k.kind) && (
                <span className="flex items-center gap-1 text-xs text-zinc-500">
                  {/* 方向由后端给：CPU 过高是 ≥，即将到期是剩余天数 ≤ */}
                  {k.cmp === 'lte' ? '剩余 ≤' : '≥'}
                  <input
                    type="number"
                    className="w-20 rounded border border-black/15 bg-transparent px-1 py-0.5 text-xs dark:border-white/15"
                    value={params[k.param] ?? ''}
                    onChange={(e) =>
                      setParams({ ...params, [k.param as string]: Number(e.target.value) })
                    }
                  />
                  {k.unit}
                </span>
              )}
            </div>
          ))}
          {kinds.length === 0 && <span className="text-xs text-zinc-500">加载中…</span>}
        </div>
      </fieldset>
      <fieldset className="mt-2">
        <legend className="mb-1 text-xs text-zinc-500">发送到</legend>
        <div className="flex flex-wrap gap-x-4 gap-y-1">
          {channels.map((c) => (
            <Check
              key={c.id}
              checked={chan.includes(c.id)}
              onChange={() => toggle(chan, c.id, setChan)}
              label={`${c.name} (${c.kind})`}
            />
          ))}
          {channels.length === 0 && <span className="text-xs text-zinc-500">先建一个渠道</span>}
        </div>
      </fieldset>
      <div className="mt-2">
        <ScopePicker scope={scope} onChange={setScope} groups={groups} servers={servers} />
      </div>
      <div className="mt-2 flex items-center gap-3">
        <Check checked={resolve} onChange={setResolve} label="恢复时也通知" />
        <Btn
          kind="primary"
          onClick={() => {
            if (!name.trim()) return setErr('规则名不能为空')
            if (picked.length === 0) return setErr('至少选一个触发事件')
            if (chan.length === 0) return setErr('至少选一个发送渠道')
            const body = {
                  name: name.trim(),
                  event_kinds: picked,
                  channel_ids: chan,
                  ...fromScope(scope),
                  // 只发被勾选事件真正用得上的阈值，别把取消勾选后的残留一起提交
                  params: Object.fromEntries(
                    kinds
                      .filter((k) => picked.includes(k.kind) && k.param && params[k.param] != null)
                      .map((k) => [k.param as string, params[k.param as string]]),
                  ),
                  duration_s: duration,
                  cooldown_s: cooldown,
                  notify_resolve: resolve,
                  title_tpl: null,
                  body_tpl: null,
                  enabled: true,
            }
            run(
              () =>
                editing
                  ? put(`/api/v1/admin/notification-rules/${editing.id}`, body)
                  : post('/api/v1/admin/notification-rules', body),
              () => {
                setName('')
                setPicked([])
                setParams({})
                setChan([])
                setScope({ kind: '', ids: [] })
                onSaved()
              },
              (m) => setErr(m || null),
            )
          }}
        >
          {editing ? '保存规则' : '添加规则'}
        </Btn>
        {editing && <Btn onClick={onCancel}>取消</Btn>}
      </div>
    </Card>
  )
}
