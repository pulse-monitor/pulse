import { useState } from 'react'

/**
 * 国旗。
 *
 * 用 **SVG 图片**（flag-icons，MIT），不用 emoji：
 * - **Windows 没有国旗 emoji 字体**，🇺🇸 会渲染成 "US" 两个字母
 * - emoji 的长相随系统变（苹果 / Google / Twemoji 各不相同）
 * - SVG 到哪儿都一样，还能按需加载、各自缓存
 *
 * 换成图片之后，之前那套「画到画布上看是不是彩色」的字体探测就不需要了 ——
 * 那个判据本身也踩过坑（宽度启发式在 macOS 上误判）。
 *
 * 图片加载失败（国家代码不在 flag-icons 里、或静态文件没部署）时回落成
 * 文字角标，而不是留一个破图。
 */
export function Flag({ code, className = '' }: { code: string | null; className?: string }) {
  const [broken, setBroken] = useState(false)
  if (!code) return null
  const cc = code.trim().toLowerCase()
  if (!/^[a-z]{2}$/.test(cc)) return null
  const label = cc.toUpperCase()

  if (broken) {
    return (
      <span
        className={`inline-block rounded-sm bg-black/10 px-1 text-[10px] font-semibold leading-4 dark:bg-white/15 ${className}`}
        title={label}
      >
        {label}
      </span>
    )
  }

  return (
    <img
      src={`/flags/${cc}.svg`}
      alt={label}
      title={label}
      loading="lazy"
      decoding="async"
      onError={() => setBroken(true)}
      // 4:3 的国旗。给固定高度、宽度自适应，避免不同比例的旗把行高撑乱
      className={`inline-block h-[0.9em] w-auto rounded-[2px] align-[-0.1em] shadow-[0_0_0_0.5px_rgba(0,0,0,0.12)] ${className}`}
    />
  )
}
