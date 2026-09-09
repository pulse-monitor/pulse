/**
 * 检查构建产物。**跑在 postbuild 上**，任何构建路径都绕不过去。
 *
 * 为什么不能只在 prebuild 里查：prebuild 保证的是「public/ 生成得对」，
 * 而产物是 vite 从 public/ 拷过去的 —— 中间但凡有东西往 public/ 里塞了脏文件
 * （这个项目放在 Desktop 上，iCloud 同步会造出一堆 `xx 2.svg` 冲突副本），
 * 或者有人直接 `npx vite build` 跳过 prebuild，脏文件就一路进了部署包。
 * 真发生过：部署上去 281 面国旗，其中 10 个是垃圾。
 */
import { readdirSync, existsSync, statSync, unlinkSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const dist = fileURLToPath(new URL('../dist', import.meta.url))
const src = fileURLToPath(new URL('../node_modules/flag-icons/flags/4x3', import.meta.url))
const fail = (m) => {
  console.error(`✗ ${m}`)
  process.exitCode = 1
}

// 0. 先扫掉 iCloud 的冲突副本。
//
// 这个项目在 ~/Desktop 下、归 iCloud 同步（FXICloudDriveDesktop=1），
// 而 `vite build` 每次清空重建 dist/，正在上传时就会生成 `xx 2.svg`、`xx 3.svg`。
// **每次构建都发生**，一开始是直接判失败，结果变成每次都要手动重跑一遍。
// 这些文件百分之百是垃圾（名字里带空格加数字），直接删掉再往下验。
// 真正的问题（数量对不上、地图没生成）仍然会判失败。
const JUNK = / \d+\.[a-z0-9]+$/i
let cleaned = 0
const sweep = (d) => {
  for (const e of readdirSync(d, { withFileTypes: true })) {
    if (e.isDirectory()) sweep(`${d}/${e.name}`)
    else if (e.name === '.DS_Store' || JUNK.test(e.name)) {
      unlinkSync(`${d}/${e.name}`)
      cleaned++
    }
  }
}
if (existsSync(dist)) sweep(dist)
if (cleaned) console.log(`  · 清掉 ${cleaned} 个 iCloud 冲突副本`)

// 1. 国旗数量必须和上游一致
const flagsDir = `${dist}/flags`
if (!existsSync(flagsDir)) fail('dist/flags 不存在')
else {
  const got = readdirSync(flagsDir).length
  const want = readdirSync(src).length
  if (got !== want) fail(`dist/flags 有 ${got} 个，上游是 ${want} 个`)
  else console.log(`  ✔ 国旗 ${got} 面`)
}

// 2. 地图数据在，且不是个空壳
const geo = `${dist}/world-geo.json`
if (!existsSync(geo)) fail('dist/world-geo.json 不存在（gen-globe.mjs 没跑？）')
else {
  const kb = statSync(geo).size / 1024
  if (kb < 50) fail(`world-geo.json 只有 ${kb.toFixed(0)} KB，太小了`)
  else console.log(`  ✔ 地图数据 ${kb.toFixed(0)} KB`)
}

// 3. 复扫一遍，确认第 0 步真的清干净了
const left = []
const walk = (d) => {
  for (const e of readdirSync(d, { withFileTypes: true })) {
    if (e.isDirectory()) walk(`${d}/${e.name}`)
    else if (e.name === '.DS_Store' || JUNK.test(e.name)) left.push(`${d}/${e.name}`)
  }
}
walk(dist)
if (left.length) fail(`还有垃圾文件没清掉：${left.slice(0, 6).map((p) => p.slice(dist.length + 1)).join(', ')}`)
else console.log('  ✔ 无垃圾文件')

if (process.exitCode) {
  console.error('\n产物有问题，别部署。先跑 `npm run build` 重来（prebuild 会重新生成 public/）。')
}
