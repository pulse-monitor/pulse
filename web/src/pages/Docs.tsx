import { useMemo, useState } from 'react'
import { Link, useParams } from 'react-router-dom'
// 构建期生成：HTML 已经渲染好，运行时没有 Markdown 解析器
import { docs } from 'virtual:docs'

/**
 * 使用文档（R9）。左侧目录 + 右侧锚点 + 顶部过滤。
 *
 * **只在独立的文档站里用**（`web/docs-site`，`npm run build:docs`），
 * 不挂在面板上 —— 面板只放运行时要看的东西。所以路由前缀是 `/` 而不是 `/docs/`。
 *
 * 过滤用的是标题与小节名的子串匹配，没有引 fuse.js ——
 * 七篇文档的规模下，模糊搜索的收益抵不上那几 KB。
 */
export default function Docs() {
  const { slug } = useParams()
  const [q, setQ] = useState('')
  const page = docs.find((d) => d.slug === slug) ?? docs[0]

  const matches = useMemo(() => {
    const k = q.trim().toLowerCase()
    if (!k) return docs
    return docs.filter(
      (d) =>
        d.title.toLowerCase().includes(k) ||
        d.headings.some((h) => h.text.toLowerCase().includes(k)),
    )
  }, [q])

  if (!page) return <p className="text-sm text-zinc-500">还没有文档</p>

  return (
    <div className="flex flex-col gap-6 lg:flex-row">
      <nav className="lg:w-52 lg:shrink-0" aria-label="文档目录">
        <input
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="搜索…"
          aria-label="搜索文档"
          className="mb-2 w-full rounded-lg border border-black/15 bg-transparent px-2 py-1.5 text-sm dark:border-white/15"
        />
        <ul className="space-y-0.5">
          {matches.map((d) => (
            <li key={d.slug}>
              <Link
                to={`/${d.slug}`}
                aria-current={d.slug === page.slug ? 'page' : undefined}
                className={`block rounded-lg px-2 py-1 text-sm ${
                  d.slug === page.slug
                    ? 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-400'
                    : 'text-zinc-600 hover:bg-black/5 dark:text-zinc-400 dark:hover:bg-white/10'
                }`}
              >
                {d.title}
              </Link>
            </li>
          ))}
          {matches.length === 0 && (
            <li className="px-2 py-1 text-sm text-zinc-500">没有匹配的文档</li>
          )}
        </ul>
      </nav>

      <article className="doc min-w-0 flex-1">
        <h1 className="mb-4 text-xl font-semibold">{page.title}</h1>
        {/* html 来自仓库里自己的 Markdown，构建期渲染，且 markdown-it 关掉了内联 HTML */}
        <div dangerouslySetInnerHTML={{ __html: page.html }} />
      </article>

      {page.headings.length > 0 && (
        <nav className="hidden xl:block xl:w-44 xl:shrink-0" aria-label="本页小节">
          <p className="mb-1 text-xs text-zinc-500">本页</p>
          <ul className="space-y-0.5 border-l border-black/10 dark:border-white/10">
            {page.headings.map((h) => (
              <li key={h.id}>
                <a
                  href={`#${h.id}`}
                  className="block border-l-2 border-transparent px-2 py-0.5 text-xs text-zinc-500 hover:border-emerald-500 hover:text-zinc-900 dark:hover:text-zinc-100"
                >
                  {h.text}
                </a>
              </li>
            ))}
          </ul>
        </nav>
      )}
    </div>
  )
}
