import { describe, it, expect } from 'vitest'
import { alignSeries, lineColor, LINE_COLORS } from './align'

describe('alignSeries', () => {
  it('时间戳完全一致时原样并排', () => {
    const r = alignSeries([
      { ts: [1, 2, 3], values: [10, 20, 30] },
      { ts: [1, 2, 3], values: [11, 21, 31] },
    ])
    expect(r.ts).toEqual([1, 2, 3])
    expect(r.cols).toEqual([
      [10, 20, 30],
      [11, 21, 31],
    ])
  })

  it('缺的点补 null 而不是 0 —— 0 毫秒延迟是假消息', () => {
    const r = alignSeries([
      { ts: [1, 3], values: [10, 30] },
      { ts: [2], values: [20] },
    ])
    expect(r.ts).toEqual([1, 2, 3])
    expect(r.cols[0]).toEqual([10, null, 30])
    expect(r.cols[1]).toEqual([null, 20, null])
  })

  it('时间轴按升序合并，输入乱序也不影响', () => {
    const r = alignSeries([{ ts: [3, 1, 2], values: [30, 10, 20] }])
    expect(r.ts).toEqual([1, 2, 3])
    expect(r.cols[0]).toEqual([10, 20, 30])
  })

  it('原始数据里的 null 照样保留', () => {
    const r = alignSeries([{ ts: [1, 2], values: [null, 20] }])
    expect(r.cols[0]).toEqual([null, 20])
  })

  it('空输入不炸', () => {
    expect(alignSeries([])).toEqual({ ts: [], cols: [] })
    expect(alignSeries([{ ts: [], values: [] }])).toEqual({ ts: [], cols: [[]] })
  })
})

describe('lineColor', () => {
  it('前几条各不相同', () => {
    const n = LINE_COLORS.length
    expect(new Set(Array.from({ length: n }, (_, i) => lineColor(i))).size).toBe(n)
  })

  it('超出表长后循环，不会返回 undefined', () => {
    expect(lineColor(LINE_COLORS.length)).toBe(LINE_COLORS[0])
    expect(lineColor(999)).toBeTruthy()
  })
})
