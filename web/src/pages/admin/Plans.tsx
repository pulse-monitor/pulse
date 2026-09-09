import { useState } from 'react'
import { del, get, post, put } from '../../api'
import { Btn, Card, Err, Field, Num, Select, Text, run, useList } from './ui'
import { CYCLES, CURRENCIES } from './consts'

interface Plan {
  id: number
  name: string
  provider: string | null
  buy_url: string | null
  review_url: string | null
  price: number | null
  currency: string | null
  cycle: string | null
  created_at: number
}

const blank = {
  name: '',
  provider: '',
  buy_url: '',
  review_url: '',
  price: null as number | null,
  currency: 'USD',
  cycle: 'monthly',
}

/**
 * 套餐：把「购买同款 / 测评链接」（R16 的最后两个字段）与价格模板放在一处，
 * 多台同款机器不用各填一遍。
 */
export default function Plans() {
  const { data, err, reload, setErr } = useList(() => get<Plan[]>('/api/v1/admin/plans'))
  const [editing, setEditing] = useState<Plan | null>(null)
  const [draft, setDraft] = useState(blank)
  const e = editing ?? draft
  const set = (p: Partial<typeof blank>) =>
    editing ? setEditing({ ...editing, ...p } as Plan) : setDraft({ ...draft, ...p })

  const save = () => {
    if (!e.name?.trim()) return setErr('套餐名不能为空')
    const body = {
      name: e.name.trim(),
      provider: e.provider?.trim() || null,
      buy_url: e.buy_url?.trim() || null,
      review_url: e.review_url?.trim() || null,
      price: e.price,
      currency: e.currency || null,
      cycle: e.cycle || null,
    }
    run(
      () => (editing ? put(`/api/v1/admin/plans/${editing.id}`, body) : post('/api/v1/admin/plans', body)),
      () => {
        setEditing(null)
        setDraft(blank)
        reload()
      },
      (m) => setErr(m || null),
    )
  }

  return (
    <div className="space-y-3">
      <Err msg={err} />
      <Card title={editing ? `编辑「${editing.name}」` : '新建套餐'}>
        <div className="grid gap-2 sm:grid-cols-3">
          <Field label="套餐名">
            <Text value={e.name} onChange={(v) => set({ name: v })} placeholder="RackNerd 1G" />
          </Field>
          <Field label="商家">
            <Text value={e.provider ?? ''} onChange={(v) => set({ provider: v })} placeholder="RackNerd" />
          </Field>
          <Field label="价格">
            <Num value={e.price} onChange={(v) => set({ price: v })} step="0.01" min="0" />
          </Field>
          <Field label="货币">
            <Select value={e.currency ?? 'USD'} onChange={(v) => set({ currency: v })} options={CURRENCIES} />
          </Field>
          <Field label="周期">
            <Select value={e.cycle ?? 'monthly'} onChange={(v) => set({ cycle: v })} options={CYCLES} />
          </Field>
          <Field label="购买链接">
            <Text value={e.buy_url ?? ''} onChange={(v) => set({ buy_url: v })} placeholder="https://…" />
          </Field>
          <Field label="测评链接">
            <Text value={e.review_url ?? ''} onChange={(v) => set({ review_url: v })} placeholder="https://…" />
          </Field>
        </div>
        <div className="mt-2 flex gap-2">
          <Btn kind="primary" onClick={save}>
            {editing ? '保存' : '添加'}
          </Btn>
          {editing && <Btn onClick={() => setEditing(null)}>取消</Btn>}
        </div>
      </Card>

      <ul className="space-y-1.5">
        {(data ?? []).map((p) => (
          <li
            key={p.id}
            className="flex flex-wrap items-center gap-2 rounded-lg border border-black/10 bg-white px-3 py-2 text-sm dark:border-white/10 dark:bg-white/5"
          >
            <span className="font-medium">{p.name}</span>
            <span className="text-xs text-zinc-500">{p.provider}</span>
            <span className="text-xs tabular-nums">
              {p.price != null ? `${p.price} ${p.currency} / ${p.cycle}` : '未填价格'}
            </span>
            {p.buy_url && (
              <a href={p.buy_url} target="_blank" rel="noreferrer noopener" className="text-xs text-emerald-600 underline">
                购买
              </a>
            )}
            {p.review_url && (
              <a href={p.review_url} target="_blank" rel="noreferrer noopener" className="text-xs text-emerald-600 underline">
                测评
              </a>
            )}
            <span className="ml-auto flex gap-1">
              <Btn onClick={() => setEditing(p)}>编辑</Btn>
              <Btn
                kind="danger"
                onClick={() =>
                  confirm(`删除套餐「${p.name}」？引用它的机器会保留自己的账单，不受影响。`) &&
                  run(() => del(`/api/v1/admin/plans/${p.id}`), reload, (m) => setErr(m || null))
                }
              >
                删除
              </Btn>
            </span>
          </li>
        ))}
        {data?.length === 0 && <li className="text-sm text-zinc-500">还没有套餐</li>}
      </ul>
    </div>
  )
}
