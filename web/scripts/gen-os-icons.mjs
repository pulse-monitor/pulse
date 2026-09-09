/**
 * 从 simple-icons 提取用得到的几个发行版 logo，生成 `src/components/os-logos.ts`。
 *
 * **构建期跑一次，simple-icons 是 devDependency，不进运行时包** ——
 * 整包有 3000 多个图标（好几 MB），我们只要十几个。
 *
 * 图标本身是 CC0，路径数据可以直接内联。品牌名与标志的商标权归各自所有者，
 * 这里只作为「标识这台机器跑什么系统」的功能性使用。
 *
 * 用法：node scripts/gen-os-icons.mjs
 */
import { writeFileSync } from 'node:fs'
import * as si from 'simple-icons'

// id → simple-icons 的导出名。id 与 detectOs() 的返回值一一对应。
const WANT = {
  debian: 'siDebian',
  ubuntu: 'siUbuntu',
  alpine: 'siAlpinelinux',
  arch: 'siArchlinux',
  fedora: 'siFedora',
  centos: 'siCentos',
  rocky: 'siRockylinux',
  alma: 'siAlmalinux',
  redhat: 'siRedhat',
  suse: 'siOpensuse',
  macos: 'siApple',
  freebsd: 'siFreebsd',
  openwrt: 'siOpenwrt',
  linux: 'siLinux',
}

// simple-icons 里**没有 Windows**（微软要求下架了自家品牌图标）。
// 四格窗形状简单，手绘一条路径即可，不会画错。
const MANUAL = {
  windows: {
    hex: '#0078D4',
    title: 'Windows',
    path: 'M0 3.449L9.75 2.1v9.451H0m10.949-9.602L24 0v11.4H10.949M0 12.6h9.75v9.451L0 20.699M10.949 12.6H24V24l-12.9-1.801',
  },
}

// 有些品牌色在浅色背景上看不见（Apple/AlmaLinux 是纯黑），换一个能看清的
const OVERRIDE = { macos: '888888', alma: '0F4266' }

const out = []
for (const [id, m] of Object.entries(MANUAL)) {
  out.push(`  ${id}: { hex: '${m.hex}', title: ${JSON.stringify(m.title)}, path: ${JSON.stringify(m.path)} },`)
}
for (const [id, key] of Object.entries(WANT)) {
  const icon = si[key]
  if (!icon) {
    console.error(`⚠️  simple-icons 里没有 ${key}，跳过 ${id}`)
    continue
  }
  out.push(`  ${id}: { hex: '#${OVERRIDE[id] ?? icon.hex}', title: ${JSON.stringify(icon.title)}, path: ${JSON.stringify(icon.path)} },`)
}

writeFileSync(
  new URL('../src/components/os-logos.ts', import.meta.url),
  `// 由 scripts/gen-os-icons.mjs 自动生成，**不要手改**。
// 数据来自 simple-icons（CC0）。改动请改脚本后重新生成。
export interface OsLogo {
  hex: string
  title: string
  /** 24×24 viewBox 的单条路径 */
  path: string
}

export const OS_LOGOS: Record<string, OsLogo> = {
${out.join('\n')}
}
`,
)
console.log(`已生成 ${out.length} 个图标`)
