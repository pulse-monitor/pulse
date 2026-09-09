import { useCallback, useEffect, useState } from 'react'

/** 后台各页共用的表单原语。刻意做得很薄 —— 后台不需要设计感，需要的是好改。 */

export const inputCls =
  'w-full rounded-lg border border-black/15 bg-transparent px-2 py-1.5 text-sm ' +
  'dark:border-white/15 disabled:opacity-50'

export function Field({
  label,
  hint,
  children,
}: {
  label: string
  hint?: string
  children: React.ReactNode
}) {
  return (
    <label className="block">
      <span className="mb-1 block text-xs text-zinc-500">{label}</span>
      {children}
      {hint && <span className="mt-0.5 block text-[11px] text-zinc-400">{hint}</span>}
    </label>
  )
}

export function Text({
  value,
  onChange,
  ...rest
}: {
  value: string
  onChange: (v: string) => void
} & Omit<React.InputHTMLAttributes<HTMLInputElement>, 'value' | 'onChange'>) {
  return (
    <input {...rest} className={inputCls} value={value} onChange={(e) => onChange(e.target.value)} />
  )
}

export function Num({
  value,
  onChange,
  ...rest
}: {
  value: number | null
  onChange: (v: number | null) => void
} & Omit<React.InputHTMLAttributes<HTMLInputElement>, 'value' | 'onChange'>) {
  return (
    <input
      {...rest}
      type="number"
      className={inputCls}
      value={value ?? ''}
      // 空串要变成 null 而不是 0 —— 「没填」和「填了 0」在账单/流量里是两回事
      onChange={(e) => onChange(e.target.value === '' ? null : Number(e.target.value))}
    />
  )
}

export function Select({
  value,
  onChange,
  options,
}: {
  value: string
  onChange: (v: string) => void
  options: [string, string][]
}) {
  return (
    <select className={inputCls} value={value} onChange={(e) => onChange(e.target.value)}>
      {options.map(([v, l]) => (
        <option key={v} value={v}>
          {l}
        </option>
      ))}
    </select>
  )
}

export function Check({
  checked,
  onChange,
  label,
}: {
  checked: boolean
  onChange: (v: boolean) => void
  label: string
}) {
  return (
    <label className="flex items-center gap-2 text-sm">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      {label}
    </label>
  )
}

export function Btn({
  kind = 'ghost',
  ...rest
}: { kind?: 'primary' | 'ghost' | 'danger' } & React.ButtonHTMLAttributes<HTMLButtonElement>) {
  const cls = {
    primary: 'bg-emerald-600 text-white hover:bg-emerald-700',
    ghost: 'border border-black/15 hover:bg-black/5 dark:border-white/15 dark:hover:bg-white/10',
    danger: 'text-red-500 hover:bg-red-500/10',
  }[kind]
  return (
    <button
      type="button"
      {...rest}
      className={`rounded-lg px-3 py-1.5 text-sm font-medium disabled:opacity-50 ${cls} ${rest.className ?? ''}`}
    />
  )
}

export function Card({ title, children }: { title?: string; children: React.ReactNode }) {
  return (
    <section className="rounded-xl border border-black/10 bg-white p-3 dark:border-white/10 dark:bg-white/5">
      {title && <h3 className="mb-2 text-sm font-medium">{title}</h3>}
      {children}
    </section>
  )
}

/** unix 秒 ↔ `<input type=date>` 的 `YYYY-MM-DD`。空值双向都是 null。 */
export const toDate = (ts: number | null | undefined): string =>
  ts ? new Date(ts * 1000).toISOString().slice(0, 10) : ''
export const fromDate = (s: string): number | null =>
  s ? Math.floor(new Date(`${s}T00:00:00Z`).getTime() / 1000) : null

/**
 * 拉取 + 重载 + 错误的最小封装。
 *
 * 错误一律**显示出来**，不做静默 catch —— 后台操作失败却看不到原因，
 * 比直接报错更难排查。
 */
export function useList<T>(fetcher: () => Promise<T>, deps: unknown[] = []) {
  const [data, setData] = useState<T | null>(null)
  const [err, setErr] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const reload = useCallback(() => {
    setBusy(true)
    return fetcher()
      .then((d) => {
        setData(d)
        setErr(null)
      })
      .catch((e) => setErr(e instanceof Error ? e.message : String(e)))
      .finally(() => setBusy(false))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps)
  useEffect(() => {
    reload()
  }, [reload])
  return { data, err, busy, reload, setErr }
}

/** 把一次写操作包起来：出错显示、成功后重载。 */
export const run = async (
  fn: () => Promise<unknown>,
  after: () => void,
  onErr: (m: string) => void,
) => {
  try {
    await fn()
    onErr('')
    after()
  } catch (e) {
    onErr(e instanceof Error ? e.message : String(e))
  }
}

export function Err({ msg }: { msg: string | null }) {
  if (!msg) return null
  return (
    <p role="alert" className="rounded-lg bg-red-500/10 px-2 py-1 text-sm text-red-600 dark:text-red-400">
      {msg}
    </p>
  )
}
