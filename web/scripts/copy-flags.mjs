/**
 * 把 flag-icons 的国旗 SVG 拷进 `public/flags/`。
 *
 * 为什么用 SVG 图片而不是 emoji：
 * - **Windows 根本没有国旗 emoji 字体**，🇺🇸 会渲染成 "US" 两个字母
 * - emoji 的样式随系统变（苹果、Google、Twemoji 长得都不一样）
 * - SVG 到哪儿都一样，而且可以按需加载
 *
 * 为什么不打进包里：271 面旗一共 2.7 MB，其中塞尔维亚一面就 180 KB（国徽画得细）。
 * 作为静态文件放着，浏览器只会取实际用到的那几面，还能各自缓存。
 *
 * 拷贝而不是直接引用 node_modules：产物要能独立部署，不能依赖装过依赖的机器。
 * `public/flags/` 已进 .gitignore —— 它是生成物。
 *
 * 数据来自 https://github.com/lipis/flag-icons（MIT）。
 */
import { cpSync, mkdirSync, readdirSync, rmSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const src = fileURLToPath(new URL('../node_modules/flag-icons/flags/4x3', import.meta.url))
const dst = fileURLToPath(new URL('../public/flags', import.meta.url))

rmSync(dst, { recursive: true, force: true })
mkdirSync(dst, { recursive: true })
cpSync(src, dst, { recursive: true })

// **核对数量**。曾经出现过目标目录里比源多 13 个 `xx 2.svg`
// （macOS 的重名副本），而脚本照样报「成功」—— 拷完不数一遍，
// 这种脏文件会一路混进产物和部署包里。
const from = readdirSync(src).length
const to = readdirSync(dst)
const dupes = to.filter((f) => / \d+\.svg$/.test(f))
if (dupes.length > 0) {
  throw new Error(`目标目录出现重名副本：${dupes.slice(0, 5).join(', ')}${dupes.length > 5 ? ' …' : ''}`)
}
if (to.length !== from) {
  throw new Error(`拷贝数量不符：源 ${from}，目标 ${to.length}`)
}
console.log(`已拷贝 ${to.length} 面国旗到 public/flags/`)
