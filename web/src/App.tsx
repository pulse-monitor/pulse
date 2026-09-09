import { lazy, Suspense, useEffect, useState } from 'react'
import { Link, Route, Routes } from 'react-router-dom'
import Home from './pages/Home'
import { Logo } from './components/Logo'
import { VisitorBadge } from './components/VisitorBadge'

// 只有首页在首屏关键路径上，其余全部懒加载 ——
// 公开页不该为后台和文档的代码买单
const ServerDetail = lazy(() => import('./pages/ServerDetail'))
const Login = lazy(() => import('./pages/Login'))
const Admin = lazy(() => import('./pages/Admin'))
// 使用文档不在面板里 —— 它是独立站点（web/docs-site，`npm run build:docs`）。
// 面板只放运行时要看的东西，文档跟着版本走、该由文档站承载。

const THEME_KEY = 'pulse.theme'
type Theme = 'system' | 'light' | 'dark'

export default function App() {
  const [theme, setTheme] = useState<Theme>(() => {
    try {
      return (localStorage.getItem(THEME_KEY) as Theme) || 'system'
    } catch {
      return 'system'
    }
  })

  useEffect(() => {
    const el = document.documentElement
    if (theme === 'system') delete el.dataset.theme
    else el.dataset.theme = theme
    try {
      localStorage.setItem(THEME_KEY, theme)
    } catch {
      /* 隐私模式下存不了，本次会话内生效即可 */
    }
  }, [theme])

  const cycle = () =>
    setTheme((t) => (t === 'system' ? 'light' : t === 'light' ? 'dark' : 'system'))

  return (
    // 1600 而不是 max-w-6xl（1152）：这是台监控面板不是一篇文章，
    // 50-200 台机器时宽度直接换成卡片列数 —— 1152px 上限意味着
    // 哪怕 1920 的屏幕也只排得下 3 列，右边一大片白给了浏览器。
    <div className="mx-auto max-w-[1600px] px-4 py-4 sm:px-6 lg:px-10 xl:px-14">
      <header className="mb-4 flex items-center gap-3">
        <Link to="/" className="flex items-center gap-1.5 text-lg font-semibold">
          <Logo className="text-emerald-500" />
          Pulse
        </Link>
        <nav className="ml-auto flex items-center gap-3 text-sm">
          <button
            onClick={cycle}
            className="rounded-md px-2 py-1 text-zinc-500 hover:bg-black/5 dark:hover:bg-white/10"
            aria-label={`主题：${theme}`}
            title={`主题：${theme}`}
          >
            {theme === 'dark' ? '🌙' : theme === 'light' ? '☀️' : '🌗'}
          </button>
          <Link to="/admin" className="text-zinc-500 hover:underline">
            管理
          </Link>
        </nav>
      </header>

      <main>
        <Suspense fallback={<p className="text-sm text-zinc-500">加载中…</p>}>
          <Routes>
            <Route path="/" element={<Home />} />
            <Route path="/server/:uuid" element={<ServerDetail />} />
            <Route path="/login" element={<Login />} />
            <Route path="/admin" element={<Admin />} />
            <Route path="*" element={<p className="text-sm text-zinc-500">页面不存在</p>} />
          </Routes>
        </Suspense>
      </main>

      <Footer />
    </div>
  )
}

/**
 * 页脚。访客信息做成居中的一条，而不是浮在角落 ——
 * 那个位置容易挡住卡片，而且这些信息本来就属于「页面元信息」。
 */
function Footer() {
  return (
    <footer className="mt-8 flex flex-col items-center gap-2 border-t border-black/5 pt-4 text-xs text-zinc-500 dark:border-white/10">
      <VisitorBadge />
      <p>
        Powered by{' '}
        <a
          href="https://github.com/"
          target="_blank"
          rel="noreferrer noopener"
          className="font-medium text-zinc-600 hover:text-emerald-600 dark:text-zinc-400"
        >
          Pulse
        </a>
      </p>
    </footer>
  )
}
