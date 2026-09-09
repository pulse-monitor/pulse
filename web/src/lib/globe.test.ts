import { describe, it, expect } from 'vitest'
import { toVec3, basis, projectLonLat, horizonCross, ringPath, prepare, countryPath, type View } from './globe'
import { pathArea } from './globe.testutil'

/** 半径 100、圆心 (100,100) 的球，视角对着 (0°E, 0°N)。 */
const V: View = { cx: 100, cy: 100, r: 100, lambda0: 0, phi0: 0 }
const near = (a: number, b: number, tol = 1e-6) => expect(Math.abs(a - b)).toBeLessThan(tol)

describe('toVec3', () => {
  it('本初子午线赤道点在 +X', () => {
    const [x, y, z] = toVec3([0, 0])
    near(x, 1); near(y, 0); near(z, 0)
  })
  it('北极在 +Z', () => {
    const [, , z] = toVec3([0, 90])
    near(z, 1)
  })
  it('90°E 赤道点在 +Y', () => {
    const [x, y] = toVec3([90, 0])
    near(x, 0); near(y, 1)
  })
  it('都是单位向量', () => {
    for (const p of [[0, 0], [45, 30], [-120, -60], [180, 89]] as const) {
      near(Math.hypot(...toVec3(p as [number, number])), 1)
    }
  })
})

describe('basis', () => {
  it('三个基向量两两正交且都是单位长', () => {
    for (const view of [V, { ...V, lambda0: 37, phi0: -22 }]) {
      const { n, e, u } = basis(view)
      const d = (a: number[], b: number[]) => a[0]! * b[0]! + a[1]! * b[1]! + a[2]! * b[2]!
      near(d(n, e), 0); near(d(n, u), 0); near(d(e, u), 0)
      near(Math.hypot(...n), 1); near(Math.hypot(...e), 1); near(Math.hypot(...u), 1)
    }
  })
  it('赤道视角下正北就是 +Z（北在上）', () => {
    const { u } = basis(V)
    near(u[0], 0); near(u[1], 0); near(u[2], 1)
  })
})

describe('project', () => {
  it('正对视点的那个点落在圆心，深度为 1', () => {
    const p = projectLonLat([0, 0], V)
    near(p.x, 100); near(p.y, 100); near(p.depth, 1)
  })

  // 这一条锁的是**方向约定**。下面几条测试即使把 x 或 y 的符号写反也照样通过，
  // 只有这里会炸 —— 之前那版地球就是栽在符号上，查了三轮。
  it('北在上、东在右', () => {
    const north = projectLonLat([0, 45], V)
    expect(north.y).toBeLessThan(100) // 屏幕 y 朝下，所以「上」= 更小
    const east = projectLonLat([45, 0], V)
    expect(east.x).toBeGreaterThan(100)
    const west = projectLonLat([-45, 0], V)
    expect(west.x).toBeLessThan(100)
    const south = projectLonLat([0, -45], V)
    expect(south.y).toBeGreaterThan(100)
  })

  it('背面的点深度为负', () => {
    expect(projectLonLat([180, 0], V).depth).toBeLessThan(0)
    expect(projectLonLat([120, 0], V).depth).toBeLessThan(0)
  })

  it('正好 90° 处深度为 0（落在地平线上），且在圆周上', () => {
    const p = projectLonLat([90, 0], V)
    near(p.depth, 0)
    near(Math.hypot(p.x - 100, p.y - 100), 100)
  })

  it('投影点永远不会跑到球外', () => {
    for (let lon = -180; lon <= 180; lon += 7) {
      for (let lat = -90; lat <= 90; lat += 7) {
        const p = projectLonLat([lon, lat], V)
        expect(Math.hypot(p.x - 100, p.y - 100)).toBeLessThanOrEqual(100 + 1e-9)
      }
    }
  })

  it('转动视角时点跟着往左跑（经度增大 = 转到眼前）', () => {
    const a = projectLonLat([30, 0], V).x
    const b = projectLonLat([30, 0], { ...V, lambda0: 10 }).x
    expect(b).toBeLessThan(a)
  })
})

describe('horizonCross', () => {
  it('交点正好落在地平线上（深度 0）且是单位向量', () => {
    const b = basis(V)
    const a = toVec3([60, 0])
    const c = toVec3([120, 0])
    const da = a[0] * b.n[0] + a[1] * b.n[1] + a[2] * b.n[2]
    const dc = c[0] * b.n[0] + c[1] * b.n[1] + c[2] * b.n[2]
    const m = horizonCross(a, c, da, dc)
    near(Math.hypot(...m), 1)
    near(m[0] * b.n[0] + m[1] * b.n[1] + m[2] * b.n[2], 0, 1e-9)
  })
})

describe('ringPath', () => {
  const square = (lon: number, lat: number, s = 5): [number, number][] => [
    [lon - s, lat - s], [lon + s, lat - s], [lon + s, lat + s], [lon - s, lat + s],
  ]

  it('整个在正面 → 普通闭合多边形，没有圆弧', () => {
    const d = ringPath(square(0, 0), V)
    expect(d).toMatch(/^M/)
    expect(d).toMatch(/Z$/)
    expect(d).not.toContain('A')
  })

  it('整个在背面 → 空串（不画）', () => {
    expect(ringPath(square(180, 0), V)).toBe('')
  })

  it('跨地平线 → 用圆弧接回去，而不是拉直线', () => {
    const d = ringPath(square(90, 0, 10), V)
    expect(d).not.toBe('')
    expect(d).toContain('A') // 有圆弧
    expect(d).toMatch(/Z$/)
  })

  it('横跨半个地球的环也不会被削平：弧上取的点都在球面上', () => {
    // 一条从 -80°E 绕到 +80°E 的长条，必定跨两次地平线
    const ring: [number, number][] = []
    for (let lon = -170; lon <= 170; lon += 10) ring.push([lon, 10])
    for (let lon = 170; lon >= -170; lon -= 10) ring.push([lon, -10])
    const d = ringPath(ring, V)
    expect(d).toContain('A')
    // 路径里的每个坐标都必须在圆内（含边界）
    for (const m of d.matchAll(/(-?[\d.]+),(-?[\d.]+)(?=[A-Z]|$|L)/g)) {
      const x = Number(m[1]); const y = Number(m[2])
      if (!Number.isFinite(x) || !Number.isFinite(y)) continue
      expect(Math.hypot(x - 100, y - 100)).toBeLessThanOrEqual(100.5)
    }
  })

  it('少于三个点的环直接丢掉', () => {
    expect(ringPath([[0, 0], [1, 1]], V)).toBe('')
  })

  it('绕地球转一圈，任何角度都不会产生 NaN', () => {
    const ring = square(0, 20, 15)
    for (let a = 0; a < 360; a += 5) {
      const d = ringPath(ring, { ...V, lambda0: a })
      expect(d).not.toContain('NaN')
    }
  })
})

describe('prepare / countryPath', () => {
  it('预转向量后画出来的路径，和直接用经纬度画的一模一样', () => {
    const rings: [number, number][][] = [
      [[-5, -5], [5, -5], [5, 5], [-5, 5]],
      [[80, 0], [100, 0], [100, 10], [80, 10]], // 这个跨地平线
    ]
    const prepped = prepare({ XX: rings })
    const view = { ...V, lambda0: 25 }
    expect(countryPath(prepped.XX!.rings, view)).toBe(
      rings.map((r) => ringPath(r, view)).filter(Boolean).join(' '),
    )
  })
})


describe('跨地平线时圆弧的绕行方向', () => {
  const DISC = Math.PI * 100 * 100 // 整个球的面积

  it('横跨地平线的方块，填的是它自己而不是整个地球', () => {
    // 一个 20° 见方的块，放在地平线上（视点在 0°E，块在 80..100°E）
    const ring: [number, number][] = [[80, -10], [100, -10], [100, 10], [80, 10]]
    const d = ringPath(ring, V)
    const a = pathArea(d, 100, 100, 100)
    expect(a).toBeGreaterThan(0)
    expect(a).toBeLessThan(DISC * 0.2) // 绕反了这里会是 0.9 以上
  })

  it('绕向相反的同一个块，面积一样（数据绕向不该影响结果）', () => {
    const cw: [number, number][] = [[80, -10], [100, -10], [100, 10], [80, 10]]
    const ccw = [...cw].reverse() as [number, number][]
    const a1 = pathArea(ringPath(cw, V), 100, 100, 100)
    const a2 = pathArea(ringPath(ccw, V), 100, 100, 100)
    expect(Math.abs(a1 - a2) / a1).toBeLessThan(0.02)
  })

  it('转一整圈，面积始终是块本身的量级，不会突然变成整个球', () => {
    const ring: [number, number][] = [[-10, -10], [10, -10], [10, 10], [-10, 10]]
    for (let a = 0; a < 360; a += 5) {
      const d = ringPath(ring, { ...V, lambda0: a })
      if (!d) continue
      expect(pathArea(d, 100, 100, 100)).toBeLessThan(DISC * 0.25)
    }
  })

  it('一个占了大半个球的巨大环，面积也不会超过整个球', () => {
    const ring: [number, number][] = []
    for (let lon = -180; lon < 180; lon += 10) ring.push([lon, 60])
    for (let lon = 170; lon >= -180; lon -= 10) ring.push([lon, 20])
    for (let a = 0; a < 360; a += 15) {
      const d = ringPath(ring, { ...V, lambda0: a })
      if (!d) continue
      expect(pathArea(d, 100, 100, 100)).toBeLessThanOrEqual(DISC * 1.01)
    }
  })
})
