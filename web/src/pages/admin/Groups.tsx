import { useState } from 'react'
import { del, get, post, put } from '../../api'
import { Btn, Card, Err, Field, Text, run, useList } from './ui'

interface Group {
  id: number
  name: string
  color: string | null
  icon: string | null
  sort_order: number
}

const blank = { name: '', color: '', icon: '', sort_order: 0 }

/** 分组管理（R11）。前端分组标签的数据来源。 */
export default function Groups() {
  const { data, err, reload, setErr } = useList(() => get<Group[]>('/api/v1/admin/groups'))
  const [draft, setDraft] = useState(blank)
  const [editing, setEditing] = useState<Group | null>(null)
  const e = editing ?? draft

  const set = (p: Partial<typeof blank>) =>
    editing ? setEditing({ ...editing, ...p }) : setDraft({ ...draft, ...p })

  const save = () => {
    const body = {
      name: e.name.trim(),
      color: e.color?.trim() || null,
      icon: e.icon?.trim() || null,
      sort_order: Number(e.sort_order) || 0,
    }
    if (!body.name) return setErr('分组名不能为空')
    run(
      () => (editing ? put(`/api/v1/admin/groups/${editing.id}`, body) : post('/api/v1/admin/groups', body)),
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
      <Card title={editing ? `编辑「${editing.name}」` : '新建分组'}>
        <div className="grid gap-2 sm:grid-cols-4">
          <Field label="名称">
            <Text value={e.name} onChange={(v) => set({ name: v })} placeholder="亚太" />
          </Field>
          <Field label="图标" hint="任意 emoji">
            <Text value={e.icon ?? ''} onChange={(v) => set({ icon: v })} placeholder="🌏" />
          </Field>
          <Field label="颜色" hint="CSS 颜色，留空用默认">
            <Text value={e.color ?? ''} onChange={(v) => set({ color: v })} placeholder="#16a34a" />
          </Field>
          <Field label="排序" hint="小的排前面">
            <Text
              value={String(e.sort_order ?? 0)}
              onChange={(v) => set({ sort_order: Number(v) || 0 })}
              inputMode="numeric"
            />
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
        {(data ?? []).map((g) => (
          <li
            key={g.id}
            className="flex items-center gap-2 rounded-lg border border-black/10 bg-white px-3 py-2 text-sm dark:border-white/10 dark:bg-white/5"
          >
            <span>{g.icon}</span>
            <span className="font-medium" style={g.color ? { color: g.color } : undefined}>
              {g.name}
            </span>
            <span className="text-xs text-zinc-500">#{g.sort_order}</span>
            <span className="ml-auto flex gap-1">
              <Btn onClick={() => setEditing(g)}>编辑</Btn>
              <Btn
                kind="danger"
                onClick={() =>
                  confirm(`删除分组「${g.name}」？组内机器会变成未分组，机器本身不受影响。`) &&
                  run(() => del(`/api/v1/admin/groups/${g.id}`), reload, (m) => setErr(m || null))
                }
              >
                删除
              </Btn>
            </span>
          </li>
        ))}
        {data?.length === 0 && <li className="text-sm text-zinc-500">还没有分组</li>}
      </ul>
    </div>
  )
}
