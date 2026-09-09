import { OS_LOGOS } from './os-logos'

/**
 * 系统图标。
 *
 * 路径数据由 `scripts/gen-os-icons.mjs` 从 simple-icons（CC0）提取后内联，
 * **simple-icons 是 devDependency，一个字节都不进运行时包** ——
 * 整包三千多个图标好几 MB，我们只要十几个。
 *
 * 认不出的系统返回 `null`（整个图标不显示），不画问号占位：
 * 一个认不出的图标比没有图标更让人困惑。
 */

/** 从 agent 上报的 `os` 字符串里认出发行版。大小写不敏感，按特征词匹配。 */
export function detectOs(os: string | null | undefined): string | null {
  if (!os) return null
  const s = os.toLowerCase()
  // 顺序有讲究：先匹配更具体的。
  // 比如 "Ubuntu" 的 os 串里常同时带 "linux"，"CentOS Stream" 里带 "centos"
  for (const [needle, id] of [
    ['ubuntu', 'ubuntu'],
    ['debian', 'debian'],
    ['centos', 'centos'],
    ['rocky', 'rocky'],
    ['almalinux', 'alma'],
    ['fedora', 'fedora'],
    ['red hat', 'redhat'],
    ['rhel', 'redhat'],
    ['alpine', 'alpine'],
    ['arch', 'arch'],
    ['opensuse', 'suse'],
    ['suse', 'suse'],
    ['windows', 'windows'],
    ['darwin', 'macos'],
    ['macos', 'macos'],
    ['mac os', 'macos'],
    ['freebsd', 'freebsd'],
    ['openwrt', 'openwrt'],
    // 兜底：认得出是 Linux 但认不出发行版
    ['linux', 'linux'],
  ] as const) {
    if (s.includes(needle)) return id
  }
  return null
}

export function OsIcon({ os, className = '' }: { os: string | null; className?: string }) {
  const id = detectOs(os)
  const logo = id ? OS_LOGOS[id] : undefined
  if (!logo) return null
  return (
    <svg
      viewBox="0 0 24 24"
      className={`inline-block h-3.5 w-3.5 shrink-0 ${className}`}
      fill={logo.hex}
      role="img"
      aria-label={os ?? logo.title}
    >
      <title>{os ?? logo.title}</title>
      <path d={logo.path} />
    </svg>
  )
}
