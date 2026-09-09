// M7 验收：前端在 N 台机器实时推送下的 LCP 与稳态 CPU。
//
// 方法学两处要点，都踩过坑：
// 1. LCP 在**同一个浏览器实例内重复导航**测，并丢掉第一次。每次新启一个
//    Chromium 再立刻导航，浏览器/GPU 进程的冷启动会被算进 navigationStart，
//    实测能把 LCP 从 200ms 抬到 6s —— 那测的是 Playwright 不是页面。
//    真实场景是用户浏览器已经开着，导航过去。
// 2. CPU 取 CDP Performance.getMetrics 的 TaskDuration 差值（渲染进程真正
//    忙的秒数 / 墙钟）。不用 ps：macOS 上 ps %cpu 是进程生命周期均值。
import { chromium } from 'playwright'
const url = process.argv[2] || 'http://127.0.0.1:25831/'
const windowS = Number(process.argv[3] || 60)
const runs = Number(process.argv[4] || 5)

const browser = await chromium.launch({ headless: process.env.HEADED !== '1' })
const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 })
await page.addInitScript(() => {
  window.__lcp = 0
  new PerformanceObserver((l) => {
    for (const e of l.getEntries()) window.__lcp = e.startTime
  }).observe({ type: 'largest-contentful-paint', buffered: true })
})
const errors = []
page.on('console', (m) => m.type() === 'error' && errors.push(m.text()))
page.on('pageerror', (e) => errors.push(String(e)))

const cdp = await page.context().newCDPSession(page)
await cdp.send('Network.enable')
// 冷缓存：LCP 预算针对的是首次访问
await cdp.send('Network.setCacheDisabled', { cacheDisabled: true })
await page.goto('about:blank')

const lcps = []
let cards = 0
for (let i = 0; i <= runs; i++) {
  await page.goto(url, { waitUntil: 'load' })
  await page.waitForSelector('.card-grid > *')
  cards = await page.locator('.card-grid > *').count()
  const lcp = await page.evaluate(() => window.__lcp)
  if (i > 0) lcps.push(lcp) // 丢掉第一次：浏览器进程还在热身
  await page.goto('about:blank')
}
lcps.sort((a, b) => a - b)
console.log(`卡片数 ${cards}`)
console.log(
  `LCP 冷缓存 ${runs} 次: 中位 ${lcps[Math.floor(runs / 2)].toFixed(0)}ms  ` +
    `最大 ${lcps[runs - 1].toFixed(0)}ms  [${lcps.map((v) => v.toFixed(0)).join(', ')}]`
)

await page.goto(url, { waitUntil: 'load' })
await page.waitForSelector('.card-grid > *')
await cdp.send('Performance.enable')
const read = async () =>
  Object.fromEntries((await cdp.send('Performance.getMetrics')).metrics.map((m) => [m.name, m.value]))
const a = await read()
await page.waitForTimeout(windowS * 1000)
const b = await read()
const busy = b.TaskDuration - a.TaskDuration
const wall = b.Timestamp - a.Timestamp
console.log(`稳态 CPU ${((busy / wall) * 100).toFixed(1)}% 单核  (${windowS}s 窗口, 忙 ${busy.toFixed(2)}s)`)
console.log(`JS 堆 ${(b.JSHeapUsedSize / 1048576).toFixed(1)} MiB   节点数 ${b.Nodes}`)
console.log(errors.length ? `控制台错误 ${errors.length}: ${errors.slice(0, 3).join(' | ')}` : '无控制台错误')
await browser.close()
