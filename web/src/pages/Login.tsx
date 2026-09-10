import { useEffect, useState } from 'react'
import { useLocation, useNavigate } from 'react-router-dom'
import { api, hasToken, setToken } from '../api'

const FIELD =
  'mt-1 w-full rounded-lg border border-black/15 bg-transparent px-2 py-1.5 dark:border-white/15'

/**
 * 登录页，同时兼任首次初始化页。
 *
 * 两件事放在一个路由里而不是拆成 /setup：面板到底初始化没有是**服务端**
 * 才知道的事，拆开的话不管进哪个都可能是错的，还得互相跳一次。
 */
export default function Login() {
  const nav = useNavigate()
  const loc = useLocation()
  // 从哪儿被弹过来的，登录成功就回哪儿去
  const from = (loc.state as { from?: string } | null)?.from || '/admin'
  const [checking, setChecking] = useState(true)
  const [setupNeeded, setSetupNeeded] = useState(false)
  const [u, setU] = useState('')
  const [p, setP] = useState('')
  const [p2, setP2] = useState('')
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
      .catch(async () => {
        // 没登录态才去问要不要初始化。问不出来就按「已初始化」处理 ——
        // 宁可显示登录框，也不要给一个可能已经失效的初始化表单
        try {
          const s = await api.setupStatus()
          setSetupNeeded(s.needed)
          if (!s.needed) setU('admin')
        } catch {
          setU('admin')
        }
        setChecking(false)
      })
  }, [])

  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    if (setupNeeded && p !== p2) {
      setErr('两次输入的密码不一致')
      return
    }
    setBusy(true)
    setErr(null)
    try {
      const r = setupNeeded ? await api.setup(u.trim(), p) : await api.login(u, p)
      setToken(r.access_token)
      nav(from, { replace: true })
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e)
      setErr(msg)
      // 初始化时撞上 409 = 别人抢先建好了。退回登录表单，别让人对着
      // 一个已经失效的表单反复试
      if (setupNeeded && /已经初始化/.test(msg)) {
        setSetupNeeded(false)
        setP2('')
      }
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
      <h1 className="text-lg font-medium">{setupNeeded ? '设置管理员账号' : '登录'}</h1>
      {setupNeeded && (
        <p className="text-xs text-zinc-500">
          这是面板的首次启动，请设定你自己的用户名和密码。设定后本页面即变为登录页。
        </p>
      )}
      <label className="block text-sm">
        <span className="text-zinc-500">用户名</span>
        <input
          value={u}
          onChange={(e) => setU(e.target.value)}
          autoComplete="username"
          minLength={setupNeeded ? 3 : undefined}
          maxLength={setupNeeded ? 32 : undefined}
          required={setupNeeded}
          className={FIELD}
        />
      </label>
      <label className="block text-sm">
        <span className="text-zinc-500">密码</span>
        <input
          type="password"
          value={p}
          onChange={(e) => setP(e.target.value)}
          autoComplete={setupNeeded ? 'new-password' : 'current-password'}
          minLength={setupNeeded ? 8 : undefined}
          required={setupNeeded}
          className={FIELD}
        />
      </label>
      {setupNeeded && (
        <label className="block text-sm">
          <span className="text-zinc-500">确认密码</span>
          <input
            type="password"
            value={p2}
            onChange={(e) => setP2(e.target.value)}
            autoComplete="new-password"
            required
            className={FIELD}
          />
        </label>
      )}
      {err && <p className="text-sm text-red-500">{err}</p>}
      <button
        disabled={busy}
        className="w-full rounded-lg bg-emerald-600 py-1.5 text-sm font-medium text-white disabled:opacity-50"
      >
        {busy ? (setupNeeded ? '创建中…' : '登录中…') : setupNeeded ? '创建并进入' : '登录'}
      </button>
      {setupNeeded ? (
        <p className="text-xs text-amber-600 dark:text-amber-500">
          在你完成设置之前，任何能打开这个地址的人都能抢先创建管理员。请立即完成。
        </p>
      ) : (
        <p className="text-xs text-zinc-500">忘记密码只能重置，数据库里只有哈希。</p>
      )}
    </form>
  )
}
