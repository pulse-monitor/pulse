import { useEffect, useState } from 'react'
import { api } from '../api'
import * as f from '../lib/format'

const DISMISS_KEY = 'pulse.visitor.dismissed'

/**
 * 访客信息条（R14）。
 *
 * 放在页脚居中，而不是浮在右下角 —— 浮窗会挡住卡片，而且这些信息
 * 本来就属于「页面元信息」。
 *
 * 隐私说明：这些信息**不被记录**，只在请求内计算并返回一次
 * 。字段缺失时不显示那一段，而不是显示「未知」。
 */
export function VisitorBadge() {
  const [v, setV] = useState<Awaited<ReturnType<typeof api.visitor>> | null>(null)
  const [hidden, setHidden] = useState(() => {
    try {
      return localStorage.getItem(DISMISS_KEY) === '1'
    } catch {
      return false
    }
  })
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000))

  useEffect(() => {
    if (hidden) return
    // 接口 404 = 站点关掉了这个功能，静默不渲染
    api.visitor().then(setV).catch(() => setV(null))
  }, [hidden])

  useEffect(() => {
    const t = setInterval(() => setNow(Math.floor(Date.now() / 1000)), 1000)
    return () => clearInterval(t)
  }, [])

  if (hidden || !v) return null

  const dismiss = () => {
    try {
      localStorage.setItem(DISMISS_KEY, '1')
    } catch {
      /* 隐私模式下存不了，本次会话内隐藏即可 */
    }
    setHidden(true)
  }

  // 有什么显示什么。拿不到的字段直接不占位，不显示「未知」
  const parts = [
    v.ip,
    v.isp || null,
    [v.os, v.browser].filter(Boolean).join(' ') || null,
    f.time(now),
  ].filter(Boolean) as string[]

  return (
    <aside
      className="flex max-w-full flex-wrap items-center justify-center gap-x-2 gap-y-1
                 rounded-full border border-black/10 bg-white/70 px-3 py-1.5
                 backdrop-blur dark:border-white/10 dark:bg-white/5"
      title="这些信息只在本次请求内计算并返回，不会被记录"
    >
      {parts.map((t, i) => (
        <span key={i} className="flex items-center gap-2">
          {i > 0 && <span className="text-zinc-300 dark:text-zinc-600">|</span>}
          <span className={i === 0 ? 'font-mono' : 'tabular-nums'}>{t}</span>
        </span>
      ))}
      <button
        onClick={dismiss}
        className="ml-1 px-1 text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200"
        aria-label="关闭访客信息"
      >
        ×
      </button>
    </aside>
  )
}
