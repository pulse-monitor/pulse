import { useEffect, useState } from 'react'
import { useLocation, useNavigate } from 'react-router-dom'
import { api, hasToken, setToken } from '../api'

export default function Login() {
  const nav = useNavigate()
  const loc = useLocation()
  // 从哪儿被弹过来的，登录成功就回哪儿去
  const from = (loc.state as { from?: string } | null)?.from || '/admin'
  const [checking, setChecking] = useState(true)
  const [u, setU] = useState('admin')
  const [p, setP] = useState('')
  const [err, setErr] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  // 已登录的人不该再看到登录表单：直接进后台。
  // access token 只在内存里，刷新页面就没了，所以要先用 refresh cookie 试一次。
  useEffect(() => {
    if (hasToken()) {
      nav(from, { replace: true })
      return
    }
    api
      .refresh()
      .then((r) => {
        setToken(r.access_token)
        nav(from, { replace: true })
      })
      .catch(() => setChecking(false))
  }, [])

  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setErr(null)
    try {
      const r = await api.login(u, p)
      setToken(r.access_token)
      nav(from, { replace: true })
    } catch (e) {
      // 后端刻意不区分「用户不存在」与「密码错误」
      setErr(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  if (checking) return <p className="mt-16 text-center text-sm text-zinc-500">正在检查登录状态…</p>

  return (
    <form
      onSubmit={submit}
      className="mx-auto mt-16 max-w-xs space-y-3 rounded-xl border border-black/10 bg-white p-5 dark:border-white/10 dark:bg-white/5"
    >
      <h1 className="text-lg font-medium">登录</h1>
      <label className="block text-sm">
        <span className="text-zinc-500">用户名</span>
        <input
          value={u}
          onChange={(e) => setU(e.target.value)}
          autoComplete="username"
          className="mt-1 w-full rounded-lg border border-black/15 bg-transparent px-2 py-1.5 dark:border-white/15"
        />
      </label>
      <label className="block text-sm">
        <span className="text-zinc-500">密码</span>
        <input
          type="password"
          value={p}
          onChange={(e) => setP(e.target.value)}
          autoComplete="current-password"
          className="mt-1 w-full rounded-lg border border-black/15 bg-transparent px-2 py-1.5 dark:border-white/15"
        />
      </label>
      {err && <p className="text-sm text-red-500">{err}</p>}
      <button
        disabled={busy}
        className="w-full rounded-lg bg-emerald-600 py-1.5 text-sm font-medium text-white disabled:opacity-50"
      >
        {busy ? '登录中…' : '登录'}
      </button>
      <p className="text-xs text-zinc-500">
        首次启动时密码在服务端日志里，只打印一次。
      </p>
    </form>
  )
}
