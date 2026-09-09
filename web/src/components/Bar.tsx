import { level } from '../lib/format'

const COLOR = {
  ok: 'bg-emerald-500',
  warn: 'bg-amber-500',
  danger: 'bg-red-500',
} as const

/** 进度条。`pct` 为 null 时显示灰色空槽而不是 0% 的绿条。 */
export function Bar({ pct, className = '' }: { pct: number | null; className?: string }) {
  const known = pct !== null && Number.isFinite(pct)
  const w = known ? Math.min(100, Math.max(0, pct)) : 0
  return (
    <div
      className={`h-1.5 w-full overflow-hidden rounded-full bg-black/10 dark:bg-white/10 ${className}`}
      role="progressbar"
      aria-valuenow={known ? Math.round(w) : undefined}
      aria-valuemin={0}
      aria-valuemax={100}
    >
      {known && (
        <div
          className={`h-full rounded-full transition-[width] duration-500 ${COLOR[level(pct)]}`}
          style={{ width: `${w}%` }}
        />
      )}
    </div>
  )
}
