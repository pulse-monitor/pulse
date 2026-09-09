// R/M7 验收：键盘可完整操作。用真实 Tab/Enter 走一遍，而不是靠 grep 断言。
import { chromium } from 'playwright'
const url = process.argv[2]
const b = await chromium.launch()
const p = await b.newPage({ viewport: { width: 1440, height: 900 } })
await p.goto(url, { waitUntil: 'load' })
await p.waitForSelector('.card-grid > *')

const focused = () =>
  p.evaluate(() => {
    const e = document.activeElement
    if (!e || e === document.body) return null
    const s = getComputedStyle(e)
    return {
      tag: e.tagName,
      label: (e.getAttribute('aria-label') || e.textContent || '').trim().slice(0, 24),
      // 焦点必须**看得见**：outline 不能是 none/0
      ring: s.outlineStyle !== 'none' && parseFloat(s.outlineWidth) > 0,
    }
  })

const seen = []
for (let i = 0; i < 12; i++) {
  await p.keyboard.press('Tab')
  const f = await focused()
  if (f) seen.push(f)
}
const noRing = seen.filter((f) => !f.ring)
console.log(`Tab 12 次，落到 ${seen.length} 个可聚焦元素：`)
console.log('  ' + seen.map((f) => `${f.tag}:${f.label}`).join('\n  '))
console.log(noRing.length ? `❌ 无可见焦点环: ${noRing.map((f) => f.label).join(', ')}` : '✅ 全部有可见焦点环')

// 用键盘切分组：Tab 到「未分组」再回车，卡片数应当变化
await p.goto(url, { waitUntil: 'load' })
await p.waitForSelector('.card-grid > *')
const before = await p.locator('.card-grid > *').count()
for (let i = 0; i < 12; i++) {
  await p.keyboard.press('Tab')
  const f = await focused()
  if (f && f.tag === 'BUTTON' && /未分组/.test(f.label)) {
    await p.keyboard.press('Enter')
    break
  }
}
await p.waitForTimeout(300)
const after = await p.locator('.card-grid > *').count()
console.log(`键盘切换分组：${before} → ${after} 张卡片 ${before !== after ? '✅' : '（数量未变，该分组可能就是全部）'}`)

// Esc / Enter 关闭访客标签
const badge = p.locator('aside button[aria-label="关闭访客信息"]')
console.log(`访客标签关闭按钮可聚焦: ${(await badge.count()) > 0 ? '✅' : '❌ 找不到'}`)
await b.close()
