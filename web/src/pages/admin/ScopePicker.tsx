import { Check } from './ui'

/**
 * 作用范围选择器：全部 / 按分组 / 按机器。
 *
 * 后端一直支持这三种（`scope_kind` = 空 / `group` / `servers`），
 * 但界面此前只会提交「全部」—— 于是「只给生产机发告警」这种最常见的需求
 * 根本没法配。
 */
export type Scope = { kind: '' | 'group' | 'servers'; ids: number[] }

export function ScopePicker({
  scope,
  onChange,
  groups,
  servers,
}: {
  scope: Scope
  onChange: (s: Scope) => void
  groups: { id: number; name: string }[]
  servers: { id: number; name: string }[]
}) {
  const toggle = (id: number) =>
    onChange({
      ...scope,
      ids: scope.ids.includes(id) ? scope.ids.filter((x) => x !== id) : [...scope.ids, id],
    })

  return (
    <fieldset>
      <legend className="mb-1 text-xs text-zinc-500">作用范围</legend>
      <div className="flex flex-wrap items-center gap-1">
        {(
          [
            ['', '全部机器'],
            ['group', '按分组'],
            ['servers', '按机器'],
          ] as const
        ).map(([k, label]) => (
          <button
            key={k}
            type="button"
            onClick={() => onChange({ kind: k, ids: [] })}
            aria-pressed={scope.kind === k}
            className={`rounded-lg px-2 py-0.5 text-xs transition ${
              scope.kind === k
                ? 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-400'
                : 'text-zinc-500 hover:bg-black/5 dark:hover:bg-white/10'
            }`}
          >
            {label}
          </button>
        ))}
      </div>

      {scope.kind !== '' && (
        <div className="mt-1 flex flex-wrap gap-x-4 gap-y-1">
          {(scope.kind === 'group' ? groups : servers).map((x) => (
            <Check
              key={x.id}
              checked={scope.ids.includes(x.id)}
              onChange={() => toggle(x.id)}
              label={x.name}
            />
          ))}
          {(scope.kind === 'group' ? groups : servers).length === 0 && (
            <span className="text-xs text-zinc-500">
              还没有{scope.kind === 'group' ? '分组' : '机器'}
            </span>
          )}
        </div>
      )}
      {scope.kind !== '' && scope.ids.length === 0 && (
        <p className="mt-0.5 text-[11px] text-amber-600 dark:text-amber-500">
          一个都没选 = 这条规则不会对任何机器生效
        </p>
      )}
    </fieldset>
  )
}

/** 把 `scope_kind`/`scope_ids` 两个字段读成一个 Scope。 */
export function toScope(kind: string | null, ids: number[] | null): Scope {
  return kind === 'group' || kind === 'servers'
    ? { kind, ids: ids ?? [] }
    : { kind: '', ids: [] }
}

/** 反过来，拆成后端要的两个字段。 */
export function fromScope(s: Scope): { scope_kind: string | null; scope_ids: number[] | null } {
  return s.kind === '' ? { scope_kind: null, scope_ids: null } : { scope_kind: s.kind, scope_ids: s.ids }
}

/** 给列表用的一句话描述。 */
export function scopeText(
  kind: string | null,
  ids: number[] | null,
  groups: { id: number; name: string }[],
  servers: { id: number; name: string }[],
): string {
  if (kind !== 'group' && kind !== 'servers') return '全部机器'
  const src = kind === 'group' ? groups : servers
  const names = (ids ?? []).map((i) => src.find((x) => x.id === i)?.name ?? `#${i}`)
  if (names.length === 0) return '未选任何目标（不生效）'
  return `${kind === 'group' ? '分组' : '机器'}：${names.join('、')}`
}
