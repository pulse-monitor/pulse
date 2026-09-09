// 测试用：把 `ringPath` 的路径展开成折线，好算面积。
// 单独放一个文件是因为数据驱动的那组测试也要用。
/**
 * 把 `ringPath` 产出的路径展开成折线（圆弧按方向采样），再算面积。
 *
 * 这是唯一能判定「圆弧有没有绕反」的办法：绕反了填的是补集，
 * 面积会接近整个球（πR² ≈ 31416），而正常国家只占其中一小块。
 * 光看路径字符串、看有没有 NaN、看点在不在圆内，全都抓不到这个 bug。
 */
export function pathArea(d: string, cx: number, cy: number, R: number): number {
  const pts: [number, number][] = []
  const tok = d.match(/[MLAZ][^MLAZ]*/g) ?? []
  for (const t of tok) {
    const cmd = t[0]
    const nums = (t.slice(1).match(/-?\d+\.?\d*(?:e-?\d+)?/g) ?? []).map(Number)
    if (cmd === 'M' || cmd === 'L') {
      for (let i = 0; i + 1 < nums.length; i += 2) pts.push([nums[i]!, nums[i + 1]!])
    } else if (cmd === 'A') {
      // rx ry rot large sweep x y
      const sweep = nums[4]!
      const x = nums[5]!
      const y = nums[6]!
      const prev = pts[pts.length - 1]!
      let a0 = Math.atan2(prev[1] - cy, prev[0] - cx)
      const a1 = Math.atan2(y - cy, x - cx)
      // sweep=1 是屏幕顺时针（y 朝下时角度增大）
      let da = a1 - a0
      if (sweep === 1) { while (da < 0) da += 2 * Math.PI } else { while (da > 0) da -= 2 * Math.PI }
      const steps = Math.max(2, Math.ceil(Math.abs(da) / 0.05))
      for (let i = 1; i <= steps; i++) {
        const a = a0 + (da * i) / steps
        pts.push([cx + R * Math.cos(a), cy + R * Math.sin(a)])
      }
    }
  }
  let area = 0
  for (let i = 0; i < pts.length; i++) {
    const a = pts[i]!
    const b = pts[(i + 1) % pts.length]!
    area += a[0] * b[1] - b[0] * a[1]
  }
  return Math.abs(area) / 2
}
