import { lazy, Suspense, useEffect, useState } from 'react'
import { Navigate } from 'react-router-dom'
import { api, hasToken, setToken } from '../api'

const Servers = lazy(() => import('./admin/Servers'))
const Groups = lazy(() => import('./admin/Groups'))
const Plans = lazy(() => import('./admin/Plans'))
const PingTasks = lazy(() => import('./admin/PingTasks'))
const Notify = lazy(() => import('./admin/Notify'))
const Events = lazy(() => import('./admin/Events'))
const Rates = lazy(() => import('./admin/Rates'))

type TabId = 'servers' | 'groups' | 'plans' | 'rates' | 'ping' | 'notify' | 'events'

/**
 * 后台分区。**分组式侧边栏**，参照 Komari：
 * 一级项直接可点，多项的收在可折叠的组里。
 * 横向标签在项多起来之后会换行、挤成一团，加不了新东西。
 */
const NAV: { group?: string; items: [TabId, string, React.FC][] }[] = [
  { items: [['servers', '服务器', Servers]] },
  // 「套餐」是价格与链接的模板，真正用它的地方在服务器编辑的账单表单里
  // （选一个就把价格、货币、周期一次带入）。这里只是维护模板本身。
  { group: '资产', items: [['groups', '分组', Groups], ['plans', '套餐模板', Plans], ['rates', '汇率', Rates]] },
  { group: '监控', items: [['ping', '延迟监控', PingTasks]] },
  { group: '通知', items: [['notify', '渠道与规则', Notify], ['events', '告警事件', Events]] },
]
const TABS = NAV.flatMap((g) => g.items)

export default function Admin() {
  const [authed, setAuthed] = useState(hasToken())
  const [failed, setFailed] = useState(false)
  const [tab, setTab] = useState<TabId>(
    () => (new URLSearchParams(location.search).get('tab') as TabId) || 'servers',
  )

  useEffect(() => {
    if (authed) return
    // access token 只放内存，刷新页面就没了 —— 用 refresh cookie 换一个新的
    api
      .refresh()
      .then((r) => {
        setToken(r.access_token)
        setAuthed(true)
      })
      .catch(() => setFailed(true))
  }, [authed])

  const pick = (id: TabId) => {
    setTab(id)
    // 让「刷新后还在这一页」和浏览器前进后退都成立，但不往历史里塞一堆记录
    history.replaceState(null, '', `?tab=${id}`)
  }

  // 没登录就直接去登录页，别停在一个「请先登录」的死页面上。
  // `replace` 是必要的：否则登录成功后按浏览器后退会退回这里，
  // 又被弹去登录页，形成一个退不出去的循环。
  // `from` 让登录成功后能回到原本要去的地方。
  if (failed) return <Navigate to="/login" replace state={{ from: '/admin' + location.search }} />
  if (!authed) return <p className="text-sm text-zinc-500">正在验证登录状态…</p>

  const Active = TABS.find((t) => t[0] === tab)?.[2] ?? Servers

  return (
    // 左右布局：侧边栏固定，右侧内容滚动。加板块只是往 NAV 里多写一行，
    // 不用担心横向空间不够
    <div className="flex flex-col gap-4 md:flex-row">
      <nav
        className="md:w-44 md:shrink-0"
        aria-label="后台分区"
      >
        <div className="space-y-3">
          {NAV.map((g, gi) => (
            <div key={g.group ?? gi}>
              {g.group && (
                <div className="mb-1 px-2 text-[11px] font-medium uppercase tracking-wide text-zinc-400">
                  {g.group}
                </div>
              )}
              <ul className="space-y-0.5">
                {g.items.map(([id, label]) => (
                  <li key={id}>
                    <button
                      onClick={() => pick(id)}
                      aria-current={tab === id ? 'page' : undefined}
                      className={`w-full rounded-lg px-2.5 py-1.5 text-left text-sm transition ${
                        tab === id
                          ? 'bg-emerald-500/15 font-medium text-emerald-700 dark:text-emerald-400'
                          : 'text-zinc-600 hover:bg-black/5 dark:text-zinc-400 dark:hover:bg-white/10'
                      }`}
                    >
                      {label}
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </div>
        <button
          onClick={() => api.logout().finally(() => location.assign('/'))}
          className="mt-3 w-full rounded-lg px-2.5 py-1.5 text-left text-sm text-zinc-500 hover:bg-black/5 dark:hover:bg-white/10"
        >
          退出登录
        </button>
      </nav>

      <div className="min-w-0 flex-1">
        <Suspense fallback={<p className="text-sm text-zinc-500">加载中…</p>}>
          <Active />
        </Suspense>
      </div>
    </div>
  )
}
