import { describe, it, expect } from 'vitest'
import { existsSync, readFileSync } from 'node:fs'
import { basis, isCCW, prepare, ringPathVec, type LonLat, type View } from './globe'
import { pathArea } from './globe.testutil'

/**
 * 拿**真实的 174 个国家**跑一遍，转一整圈都不能出问题。
 *
 * 合成用例抓不到这类 bug：一开始我用「20° 见方的小块」测跨地平线，
 * 全绿；换成真实的美国（横跨 57° 经度、还带阿拉斯加十个环）一上真机，
 * 整个地球被填成一个红圆。
 *
 * 地图数据是构建期生成的（`scripts/gen-globe.mjs`），没生成就跳过，
 * 不让 `npm test` 在干净检出上失败。
 */
const FILE = new URL('../../public/world-geo.json', import.meta.url)
const has = existsSync(FILE)
const geo = has ? (JSON.parse(readFileSync(FILE, 'utf8')) as Record<string, LonLat[][]>) : {}

describe.skipIf(!has)('真实国家数据', () => {
  const prepped = prepare(geo)
  const R = 100
  const DISC = Math.PI * R * R
  const angles = [0, 30, 60, 90, 120, 150, 180, 210, 240, 270, 300, 330]

  it('数据本身是齐的', () => {
    expect(Object.keys(prepped).length).toBeGreaterThan(160)
    expect(prepped.US).toBeTruthy()
    expect(prepped.DE).toBeTruthy()
  })

  it('这份数据的环全是同一个绕向（顺时针）', () => {
    let ccw = 0
    let cw = 0
    for (const c of Object.values(prepped)) for (const r of c.rings) (isCCW(r) ? ccw++ : cw++)
    // topojson 的外环是顺时针（和 GeoJSON 规范正好相反）。
    // 既然全都一样，绕行方向其实可以写死 —— 但仍然逐环算：
    // 哪天换了数据源或者改用 GeoJSON，写死的那个方向会**静默地**
    // 把每个跨地平线的国家填成补集，而逐环算的不会。
    // 这条测试就是为了让「数据源换了」这件事有人察觉。
    expect(ccw).toBe(0)
    expect(cw).toBe(281)
  })

  it('任何国家在任何角度都不会填满整个球', () => {
    const bad: string[] = []
    for (const [cc, country] of Object.entries(prepped)) {
      for (const lambda0 of angles) {
        const view: View = { cx: R, cy: R, r: R, lambda0, phi0: 18 }
        const b = basis(view)
        let total = 0
        for (const r of country.rings) {
          const d = ringPathVec(r, view, b)
          if (d) total += pathArea(d, R, R, R)
        }
        // 最大的国家（俄罗斯）正对镜头时约占可见半球的三成。
        // 绕反了会一口气冲到 90% 以上，中间有很大的余量。
        if (total > DISC * 0.55) bad.push(`${cc}@λ=${lambda0} 占 ${((total / DISC) * 100).toFixed(0)}%`)
      }
    }
    expect(bad).toEqual([])
  })

  it('转一整圈不产生 NaN，坐标都在球内', () => {
    for (const [cc, country] of Object.entries(prepped)) {
      for (let lambda0 = 0; lambda0 < 360; lambda0 += 45) {
        const view: View = { cx: R, cy: R, r: R, lambda0, phi0: 18 }
        const b = basis(view)
        for (const r of country.rings) {
          const d = ringPathVec(r, view, b)
          expect(d, cc).not.toContain('NaN')
        }
      }
    }
  })
})
