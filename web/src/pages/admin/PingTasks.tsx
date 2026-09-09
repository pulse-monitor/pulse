import { useState } from 'react'
import { del, get, post, put } from '../../api'
import { Btn, Card, Check, Err, Field, Num, Select, Text, run, useList } from './ui'
import { PING_KINDS } from './consts'
import { fromScope, ScopePicker, scopeText, toScope, type Scope } from './ScopePicker'

interface Task {
  id: number
  name: string
  kind: string
  host: string
  port: number | null
  expect_status: number | null
  interval_s: number
  packets: number
  timeout_ms: number
  scope_kind: string | null
  scope_ids: number[] | null
  enabled: boolean
}

const blank = {
  name: '',
  kind: 'icmp',
  host: '',
  port: null as number | null,
  expect_status: null as number | null,
  interval_s: 60,
  packets: 3,
  timeout_ms: 2000,
  enabled: true,
}

/** 延迟监控地址（R6）。原始点最长保留 30 天， */
export default function PingTasks() {
  const { data, err, reload, setErr } = useList(() => get<Task[]>('/api/v1/admin/ping-tasks'))
  const groups = useList(() => get<{ id: number; name: string }[]>('/api/v1/admin/groups'))
  const servers = useList(() => get<{ id: number; name: string }[]>('/api/v1/admin/servers'))
  const [editing, setEditing] = useState<Task | null>(null)
  const [draft, setDraft] = useState(blank)
  const [scope, setScope] = useState<Scope>({ kind: '', ids: [] })
  const e = editing ?? draft
  const set = (p: Partial<typeof blank>) =>
    editing ? setEditing({ ...editing, ...p } as Task) : setDraft({ ...draft, ...p })

  const save = () => {
    if (!e.name.trim || !e.host.trim) return setErr('名称和地址都不能为空')
    const body = {
      name: e.name.trim,
      kind: e.kind,
      host: e.host.trim,
      port: e.kind === 'icmp' ? null : e.port,
      expect_status: e.kind === 'http' ? e.expect_status : null,
      interval_s: e.interval_s,
      packets: e.packets,
      timeout_ms: e.timeout_ms,
      ...fromScope(scope),
      enabled: e.enabled,
    }
    run(() =>
        editing ? put(`/api/v1/admin/ping-tasks/${editing.id}`, body) : post('/api/v1/admin/ping-tasks', body), () => {
        setEditing(null)
        setDraft(blank)
        setScope({ kind: '', ids: [] })
        reload
      },
      (m) => setErr(m || null),
    )
  }

  return (
    <div className="space-y-3">
      <Err msg={err} />
      <Card title={editing ? `编辑「${editing.name}」` : '新建监控'}>
        <div className="grid gap-2 sm:grid-cols-3">
          <Field label="名称">
            <Text value={e.name} onChange={(v) => set({ name: v })} placeholder="电信 上海" />
          </Field>
          <Field label="类型">
            <Select value={e.kind} onChange={(v) => set({ kind: v })} options={PING_KINDS} />
          </Field>
          <Field label="地址" hint={e.kind === 'http' ? '完整 URL' : '主机名或 IP'}>
            <Text
              value={e.host}
              onChange={(v) => set({ host: v })}
              placeholder={e.kind === 'http' ? 'https://example.com' : '1.1.1.1'}
            />
          </Field>
          {e.kind === 'tcp' && (
            <Field label="端口">
              <Num value={e.port} onChange={(v) => set({ port: v })} min="1" max="65535" />
            </Field>
          )}
          {e.kind === 'http' && (
            <Field label="期望状态码" hint="留空表示 2xx/3xx 都算成功">
              <Num value={e.expect_status} onChange={(v) => set({ expect_status: v })} min="100" max="599" />
            </Field>
          )}
          <Field label="间隔（秒）">
            <Num value={e.interval_s} onChange={(v) => set({ interval_s: v ?? 60 })} min="10" />
          </Field>
          <Field label="每次探测包数">
            <Num value={e.packets} onChange={(v) => set({ packets: v ?? 3 })} min="1" max="10" />
          </Field>
          <Field label="超时（毫秒）">
            <Num value={e.timeout_ms} onChange={(v) => set({ timeout_ms: v ?? 2000 })} min="100" />
          </Field>
        </div>
        <div className="mt-2">
          <ScopePicker
            scope={scope}
            onChange={setScope}
            groups={groups.data ?? []}
            servers={servers.data ?? []}
          />
        </div>
        <div className="mt-2 flex items-center gap-3">
          <Check checked={e.enabled} onChange={(v) => set({ enabled: v })} label="启用" />
          <Btn kind="primary" onClick={save}>
            {editing ? '保存' : '添加'}
          </Btn>
          {editing && <Btn onClick={() => setEditing(null)}>取消</Btn>}
        </div>
        <p className="mt-2 text-[11px] text-zinc-400">
          ICMP 需要内核允许非特权 ping（<code>net.ipv4.ping_group_range</code>）。探针不提权，
          拿不到就自动降级为 TCP，前端会显示该机器不支持 ICMP。
        </p>
      </Card>

      <ul className="space-y-1.5">
        {(data ?? []).map((t) => (
          <li
            key={t.id}
            className="flex flex-wrap items-center gap-2 rounded-lg border border-black/10 bg-white px-3 py-2 text-sm dark:border-white/10 dark:bg-white/5"
          >
            <span className={t.enabled ? '' : 'opacity-50'}>
              <span className="font-medium">{t.name}</span>{' '}
              <span className="text-xs text-zinc-500">
                {t.kind} {t.host}
                {t.port ? `:${t.port}` : ''} · {t.interval_s}s · {t.packets} 包 ·{' '}
                {scopeText(t.scope_kind, t.scope_ids, groups.data ?? [], servers.data ?? [])}
              </span>
            </span>
            {!t.enabled && <span className="text-xs text-amber-600">已停用</span>}
            <span className="ml-auto flex gap-1">
              <Btn
                onClick={() => {
                  setEditing(t)
                  setScope(toScope(t.scope_kind, t.scope_ids))
                }}
              >
                编辑
              </Btn>
              <Btn
                kind="danger"
                onClick={() =>
                  confirm(`删除监控「${t.name}」？它的历史延迟数据会一并删除。`) &&
                  run(() => del(`/api/v1/admin/ping-tasks/${t.id}`), reload, (m) => setErr(m || null))
                }
              >
                删除
              </Btn>
            </span>
          </li>
        ))}
        {data?.length === 0 && <li className="text-sm text-zinc-500">还没有延迟监控地址</li>}
      </ul>
    </div>
  )
}
