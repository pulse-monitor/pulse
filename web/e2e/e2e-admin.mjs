// 后台各板块的端到端验证：这些表单是照着后端结构体推的，
// 必须真点一遍确认请求体被接受，否则「界面写好了」是空话。
import { chromium } from 'playwright'
const base = process.argv[2]
const tag = Date.now().toString(36).slice(-5) // 每次跑用不同的名字，脚本可重复执行
const b = await chromium.launch()
const p = await b.newPage({ viewport: { width: 1280, height: 900 } })
const errs = []
p.on('console', (m) => m.type() === 'error' && errs.push(m.text()))
p.on('pageerror', (e) => errs.push(String(e)))
// 后台任何一个请求失败都要看见，不能被 UI 的 catch 吞掉
const bad = []
p.on('response', (r) => {
  if (r.url().includes('/api/') && r.status() >= 400) bad.push(`${r.status()} ${r.request().method()} ${new URL(r.url()).pathname}`)
})
const step = (s) => console.log(`  ${s}`)

await p.goto(`${base}/login`)
await p.locator('input').first().fill('admin')
await p.locator('input[type=password]').fill('testpassword123')
await p.locator('button[type=submit], form button').first().click()
await p.waitForURL(/admin/, { timeout: 10000 }).catch(() => {})
if (!/admin/.test(p.url())) await p.goto(`${base}/admin`)
await p.waitForSelector('nav[aria-label="后台分区"]')
step('登录 ✅')

for (const [tab, probe] of [
  ['服务器', 'text=添加'],
  ['分组', 'text=新建分组'],
  ['套餐', 'text=新建套餐'],
  ['延迟监控', 'text=新建监控'],
  ['通知', 'text=新建通知渠道'],
  ['事件', 'th:has-text("事件")'],
  ['汇率', 'text=人工汇率'],
]) {
  await p.getByRole('button', { name: tab, exact: true }).click()
  await p.waitForSelector(probe, { timeout: 8000 })
  step(`${tab} 页渲染 ✅`)
}

// 建分组
await p.getByRole('button', { name: '分组', exact: true }).click()
await p.waitForSelector('text=新建分组')
await p.locator('input').nth(0).fill(`E2E组${tag}`)
await p.locator('input').nth(1).fill('🧪')
await p.getByRole('button', { name: '添加' }).click()
await p.waitForSelector(`text=E2E组${tag}`, { timeout: 8000 })
step('建分组 ✅')

// 建套餐
await p.getByRole('button', { name: '套餐', exact: true }).click()
await p.waitForSelector('text=新建套餐')
await p.locator('input').nth(0).fill(`E2E套餐${tag}`)
await p.locator('input[type=number]').first().fill('9.99')
await p.getByRole('button', { name: '添加' }).click()
await p.waitForSelector(`text=E2E套餐${tag}`, { timeout: 8000 })
step('建套餐 ✅')

// 建延迟监控
await p.getByRole('button', { name: '延迟监控', exact: true }).click()
await p.waitForSelector('text=新建监控')
await p.locator('input').nth(0).fill(`E2E探测${tag}`)
await p.locator('input').nth(1).fill('1.1.1.1')
await p.getByRole('button', { name: '添加' }).click()
await p.waitForSelector(`text=E2E探测${tag}`, { timeout: 8000 })
step('建延迟监控 ✅')

// 服务器：展开编辑，存账单 + 流量 + 采集
await p.getByRole('button', { name: '服务器', exact: true }).click()
await p.waitForSelector('text=安装命令')
await p.getByRole('button', { name: '编辑' }).first().click()
await p.waitForSelector('text=保存基本信息', { timeout: 8000 })
step('展开服务器编辑面板 ✅')
for (const btn of ['保存基本信息', '保存账单', '保存流量配置', '保存采集配置']) {
  await p.getByRole('button', { name: btn }).click()
  await p.waitForTimeout(600)
  step(`${btn} ✅`)
}

// 安装命令：选项要真的进到生成的命令里。
// 先收起编辑面板 —— 它的采集配置里有同名的「启用 GPU 监控」开关，
// 两个一起在场时按标签选会选错（这条 e2e 第一次跑就是这么翻车的）。
await p.getByRole('button', { name: '服务器', exact: true }).click()
await p.waitForSelector('text=安装命令')
await p.getByRole('button', { name: '收起' }).first().click()
await p.waitForTimeout(200)
await p.getByRole('button', { name: '安装命令' }).first().click()
await p.waitForSelector('text=生成安装命令')
await p.getByLabel('禁用自动更新').check()
// 「启用 GPU 监控」在采集配置里也有一个同名开关，必须限定到安装面板这一个
await p.getByLabel('启用 GPU 监控').check()
await p.getByRole('button', { name: '生成安装命令' }).click()
await p.waitForSelector('pre', { timeout: 8000 })
const cmd = await p.locator('pre').first().textContent()
const hasOpts = /no-auto-update|disable.?auto.?update/i.test(cmd ?? '') && /gpu/i.test(cmd ?? '')
console.log(hasOpts ? '安装选项进入命令 ✅' : `❌ 安装选项没进命令:\n${cmd}`)

// 重名要给用户看得懂的话。这里**故意**再建一次同名分组，
// 因此把它产生的 409 从「失败请求」里排除 —— 它是预期结果。
await p.getByRole('button', { name: '分组', exact: true }).click()
await p.waitForSelector('text=新建分组')
await p.locator('input').nth(0).fill(`E2E组${tag}`)
await p.getByRole('button', { name: '添加' }).click()
const alert = await p.waitForSelector('[role=alert]', { timeout: 8000 }).then((h) => h.textContent())
console.log(
  /同名/.test(alert ?? '') ? `重名提示可读: 「${alert}」 ✅` : `❌ 重名提示不可读: 「${alert}」`
)
const realBad = bad.filter((x) => !x.startsWith('409'))

console.log(realBad.length ? `❌ 失败请求: ${[...new Set(realBad)].join(' | ')}` : '✅ 无失败请求')
// 上面故意制造的 409 也会被浏览器记一条 console error，那是预期的
const realErrs = errs.filter((e) => !/409|Conflict/.test(e))
console.log(realErrs.length ? `❌ 控制台错误: ${realErrs.slice(0, 4).join(' | ')}` : '✅ 无控制台错误')
await b.close()
process.exit(realBad.length || realErrs.length ? 1 : 0)
