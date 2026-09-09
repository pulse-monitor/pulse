import { chromium } from 'playwright'

const out = process.argv[2]
const url = process.env.URL || 'http://127.0.0.1:25830/'
// HEADED=1 用真实 GPU 渲染。以前地球是 cobe（WebGL），headless 的 SwiftShader
// 会把它画成一个纯黑圆 —— 同一份代码 headed 正常、headless 全黑，实测过。
// 现在地球改成纯 SVG 自己投影，**不再碰 WebGL**，headless 就够用了。
// 这个开关留着：万一以后又引入 WebGL 的东西，还得靠它。
const browser = await chromium.launch({ headless: process.env.HEADED !== '1' })

for (const [name, scheme, width] of [
  ['light', 'light', 1280],
  ['dark', 'dark', 1280],
  ['mobile', 'light', 390],
]) {
  const ctx = await browser.newContext({
    colorScheme: scheme,
    viewport: { width, height: width === 390 ? 844 : 900 },
    deviceScaleFactor: 2,
  })
  const page = await ctx.newPage()
  const errors = []
  page.on('console', (m) => m.type() === 'error' && errors.push(m.text()))
  page.on('response', (r) => {
    if (r.status() >= 400) errors.push(`${r.status()} ${new URL(r.url()).pathname}`)
  })
  page.on('pageerror', (e) => errors.push(String(e)))

  await page.goto(url, { waitUntil: 'networkidle' })
  // 等 WS 推一帧（2 秒间隔），确认实时层也活着
  await page.waitForTimeout(4000)
  await page.screenshot({ path: `${out}/${name}.png`, fullPage: true })

  if (name === 'light') {
    // 抓几个关键文本，验证渲染出来的确实是数据而不是骨架
    const txt = await page.locator('body').innerText()
    console.log('--- 页面文本（前 40 行）---')
    console.log(txt.split('\n').slice(0, 40).map((l) => '  ' + l).join('\n'))
  }
  console.log(`  ${name}: ${errors.length ? '控制台错误 ' + errors.slice(0, 3).join(' | ') : '无控制台错误'}`)
  await ctx.close()
}
await browser.close()
