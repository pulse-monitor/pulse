/**
 * 正交投影（orthographic）—— 把经纬度多边形画到球面上。
 *
 * 为什么自己写而不用 d3-geo：这里只需要「正交投影 + 地平线裁剪」两件事，
 * d3-geo 全家桶几百 KB，而下面这一百来行就够了，还能针对性地写测试。
 *
 * 坐标约定（下面每个函数都依赖它，改之前先看测试）：
 *   - 经纬度用**度**，顺序是 [经度, 纬度]，和 GeoJSON 一致
 *   - 单位球上的点：`[cosφcosλ, cosφsinλ, sinφ]`
 *   - 屏幕 y 轴**朝下**（SVG 惯例），所以北在上意味着 y 要取负
 */

export type LonLat = [number, number]
export type Vec3 = [number, number, number]

/** 视角。`lambda0` / `phi0` 是正对观察者的那个点的经纬度（度）。 */
export interface View {
  cx: number
  cy: number
  r: number
  lambda0: number
  phi0: number
}

const RAD = Math.PI / 180

export function toVec3([lon, lat]: LonLat): Vec3 {
  const la = lon * RAD
  const ph = lat * RAD
  const c = Math.cos(ph)
  return [c * Math.cos(la), c * Math.sin(la), Math.sin(ph)]
}

const dot = (a: Vec3, b: Vec3) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2]

/**
 * 视点处的三个正交基：
 *   `n` 指向观察者，`e` 是当地正东，`u` 是当地正北。
 * 一个点的屏幕横坐标就是它在 `e` 上的投影，纵坐标是在 `u` 上的投影，
 * 而在 `n` 上的投影是「深度」—— 大于 0 才在朝向我们的这半边。
 */
export function basis(view: View): { n: Vec3; e: Vec3; u: Vec3 } {
  const la = view.lambda0 * RAD
  const ph = view.phi0 * RAD
  const n: Vec3 = [Math.cos(ph) * Math.cos(la), Math.cos(ph) * Math.sin(la), Math.sin(ph)]
  const e: Vec3 = [-Math.sin(la), Math.cos(la), 0]
  // u = n × e。φ0 = 0 时应当正好是 [0,0,1]（正北朝上）
  const u: Vec3 = [-Math.sin(ph) * Math.cos(la), -Math.sin(ph) * Math.sin(la), Math.cos(ph)]
  return { n, e, u }
}

export interface Projected {
  x: number
  y: number
  /** 在 `n` 上的投影。> 0 表示在正面，= 0 正好落在地平线上 */
  depth: number
}

export function project(v: Vec3, view: View, b = basis(view)): Projected {
  return {
    x: view.cx + view.r * dot(v, b.e),
    y: view.cy - view.r * dot(v, b.u), // 屏幕 y 朝下，所以取负
    depth: dot(v, b.n),
  }
}

/** 经纬度直接投到屏幕。 */
export function projectLonLat(p: LonLat, view: View, b = basis(view)): Projected {
  return project(toVec3(p), view, b)
}

/**
 * 一条边从正面跨到背面时，求它与地平线的交点。
 *
 * 地平线是 `v·n = 0` 这个平面与球面的交线。把 a、b 按深度线性插值，
 * 得到的点满足 `v·n = 0`（代进去正好抵消），再归一化拉回球面上 ——
 * 这不是近似，是精确解：a、b 决定的大圆就在 a、b 张成的平面里，
 * 该平面与 `v·n=0` 的交线唯一。
 */
export function horizonCross(a: Vec3, b: Vec3, da: number, db: number): Vec3 {
  const t = da / (da - db)
  const m: Vec3 = [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1]), a[2] + t * (b[2] - a[2])]
  const len = Math.hypot(m[0], m[1], m[2]) || 1
  return [m[0] / len, m[1] / len, m[2] / len]
}

/**
 * 环的固有绕向：从球**外面**看它自己那一片，是逆时针就返回 true。
 *
 * 这决定跨地平线时沿地平线绕行的方向。判据用的是环**自身**的朝向
 * （Newell 法向 vs 环重心），不是相对视线的朝向 —— 这一点实测过：
 * 换成相对视线后准确率从 96/104 掉到 59/104。
 * 道理是：可见的那部分一定在朝向我们的半球上，观察者是从「外面」看它的，
 * 左右关系不会翻转，所以看到的绕向就等于固有绕向。
 */
export function isCCW(vs: Vec3[]): boolean {
  let nx = 0
  let ny = 0
  let nz = 0
  let cx = 0
  let cy = 0
  let cz = 0
  for (let i = 0; i < vs.length; i++) {
    const a = vs[i]!
    const b = vs[(i + 1) % vs.length]!
    nx += a[1] * b[2] - a[2] * b[1]
    ny += a[2] * b[0] - a[0] * b[2]
    nz += a[0] * b[1] - a[1] * b[0]
    cx += a[0]
    cy += a[1]
    cz += a[2]
  }
  return nx * cx + ny * cy + nz * cz > 0
}

/** 地平线上一点的参数角 α：`v = cosα·e + sinα·u`。 */
function horizonAngle(v: Vec3, b: { e: Vec3; u: Vec3 }): number {
  return Math.atan2(dot(v, b.u), dot(v, b.e))
}

const f = (n: number) => (Math.round(n * 100) / 100).toString()

/**
 * 把一个经纬度环转成 SVG 路径。整环都在背面时返回空串。
 *
 * 跨地平线的环要沿着**地平线圆弧**接回去，不能直接拉直线 ——
 * 俄罗斯这种横跨 170° 经度的国家，拉直线会把它齐刷刷削掉一大块。
 *
 * 圆弧的方向由**出界那一刻的走向**决定：环在出界点 E 处的切向 d，
 * 与地平线在 E 处的切向 `n × E` 点积为正，就说明该沿 α 增大的方向走。
 */
export function ringPath(ring: LonLat[], view: View, b = basis(view)): string {
  return ringPathVec(ring.map(toVec3), view, b)
}

/**
 * 同上，但吃的是**预先转好的单位向量**。
 *
 * 经纬度→向量要三次三角函数，而它和视角无关、转球时不会变。
 * 全球 174 个国家约八千个点，每帧重算就是两万多次三角函数 ——
 * 所以数据加载时用 `prepare()` 转一次，之后每帧只做点积。
 */
export function ringPathVec(vs: Vec3[], view: View, b = basis(view)): string {
  const n = vs.length
  if (n < 3) return ''

  const ds = vs.map((v) => dot(v, b.n))

  if (ds.every((d) => d <= 0)) return '' // 整个在背面
  if (ds.every((d) => d > 0)) {
    // 全在正面，普通多边形
    const pts = vs.map((v) => project(v, view, b))
    return 'M' + pts.map((p) => `${f(p.x)},${f(p.y)}`).join('L') + 'Z'
  }

  // 跨地平线：先切成若干段连续可见的「run」
  type Run = { pts: Projected[]; enter: Vec3; exit: Vec3 }
  const runs: Run[] = []
  let cur: Run | null = null

  for (let i = 0; i < n; i++) {
    const j = (i + 1) % n
    // i、j 都由 % n 得来，必在范围内
    const vi = vs[i]!
    const vj = vs[j]!
    const di = ds[i]!
    const dj = ds[j]!

    if (di > 0) {
      if (!cur) {
        // 环的起点就在正面。这一段的真正入界点在环尾巴上，循环结束后补进来
        cur = { pts: [], enter: vi, exit: vi }
      }
      cur.pts.push(project(vi, view, b))
    }

    if (di > 0 && dj <= 0) {
      // 出界
      const x = horizonCross(vi, vj, di, dj)
      cur!.pts.push(project(x, view, b))
      cur!.exit = x
      runs.push(cur!)
      cur = null
    } else if (di <= 0 && dj > 0) {
      // 入界
      const x = horizonCross(vj, vi, dj, di)
      cur = { pts: [project(x, view, b)], enter: x, exit: x }
    }
  }
  // 环是首尾相接的，如果收尾时还开着一段，把它并到第一段前面
  if (cur) {
    if (runs.length) {
      runs[0]!.pts = cur.pts.concat(runs[0]!.pts)
      runs[0]!.enter = cur.enter
    } else {
      return ''
    }
  }
  if (!runs.length) return ''

  const R = view.r
  // 整个环用同一个绕行方向：由环的固有绕向决定。
  // 逐个缺口各自判断会互相矛盾，把补集填出来（美国那次整个地球变红圆）。
  const increasing = isCCW(vs)
  const EPS = 1e-9
  const exitA = runs.map((r) => horizonAngle(r.exit, b))
  const enterA = runs.map((r) => horizonAngle(r.enter, b))

  /**
   * 从第 k 段的出界点，沿绕行方向走，**第一个**碰到的入界点。
   *
   * 注意不是「环里的下一段」—— 一个国家的可见部分可能是**好几块互不相连**的
   * （加拿大、俄罗斯、南极洲这些又大又碎的，在地平线边上会切出好几块）。
   * 按环的顺序硬串会把这些块连成一条路径，绕着地平线转好几圈，
   * 面积能到整个球的 3 倍、5 倍。
   */
  const nextRun = (k: number): { j: number; span: number } => {
    let bj = k
    let bs = Infinity
    for (let j = 0; j < runs.length; j++) {
      let da = enterA[j]! - exitA[k]!
      if (increasing) {
        while (da <= EPS) da += 2 * Math.PI
      } else {
        while (da >= -EPS) da -= 2 * Math.PI
        da = -da
      }
      if (da < bs) {
        bs = da
        bj = j
      }
    }
    return { j: bj, span: bs }
  }

  const used = new Array<boolean>(runs.length).fill(false)
  let d = ''
  for (let s0 = 0; s0 < runs.length; s0++) {
    if (used[s0]) continue
    let k = s0
    let sub = ''
    // 每段最多走一次，绕不回起点就停 —— 防着浮点误差把它变成死循环
    for (let guard = 0; guard <= runs.length; guard++) {
      used[k] = true
      sub += (sub ? 'L' : 'M') + runs[k]!.pts.map((p) => `${f(p.x)},${f(p.y)}`).join('L')
      const { j, span } = nextRun(k)
      const large = span > Math.PI ? 1 : 0
      // α 增大时点在屏幕上逆时针走（y 朝下），对应 SVG 的 sweep-flag = 0
      const sweep = increasing ? 0 : 1
      const p = project(runs[j]!.enter, view, b)
      sub += `A${f(R)},${f(R)} 0 ${large},${sweep} ${f(p.x)},${f(p.y)}`
      if (j === s0 || used[j]) break
      k = j
    }
    d += sub + 'Z'
  }
  return d
}

/** 一个国家：预转好的环，加上中心点和角半径。 */
export interface Country {
  rings: Vec3[][]
  /** 各顶点的平均方向（单位向量）。用来放标记点。 */
  center: Vec3
  /** 从中心到最远顶点的角距离（弧度）。判断这个国家在屏幕上有多大。 */
  extent: number
}

/**
 * 把整份地图数据一次性转成单位向量，之后每帧复用。
 *
 * 顺便算好中心和角半径 —— 新加坡、香港这类国家在 110m 数据里只有几个点、
 * 画出来不到一个像素，得改用标记点显示。
 */
export function prepare(data: Record<string, LonLat[][]>): Record<string, Country> {
  const out: Record<string, Country> = {}
  for (const [cc, raw] of Object.entries(data)) {
    const rings = raw.map((r) => r.map(toVec3))
    let x = 0
    let y = 0
    let z = 0
    let n = 0
    for (const r of rings)
      for (const v of r) {
        x += v[0]
        y += v[1]
        z += v[2]
        n++
      }
    const len = Math.hypot(x, y, z) || 1
    const center: Vec3 = [x / len, y / len, z / len]
    let extent = 0
    for (const r of rings)
      for (const v of r) {
        const a = Math.acos(Math.max(-1, Math.min(1, dot(v, center))))
        if (a > extent) extent = a
      }
    out[cc] = { rings, center, extent }
  }
  return out
}

/** 一个国家的所有环。 */
export function countryPath(rings: Vec3[][], view: View, b = basis(view)): string {
  return rings
    .map((r) => ringPathVec(r, view, b))
    .filter(Boolean)
    .join(' ')
}
