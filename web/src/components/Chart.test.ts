import { describe, it, expect } from 'vitest'
import { lastIdx, fmtTime, fmtNum } from './Chart'

describe('lastIdx', () => {
  const mk = (x: (number | null)[], ...ys: (number | null)[][]) => ({ data: [x, ...ys] })

  it('全都有值时就是最后一个', () => {
    expect(lastIdx(mk([1, 2, 3], [10, 20, 30]))).toBe(2)
  })

  // 这是重点：折线末尾几乎总有几个 null（最后一两个桶还没攒够数据）。
  // 直接取 length-1 的话，图例会一直显示「—」——
  // 而「不悬停就该显示最新值」正是加这个函数的理由。
  it('末尾是 null 时往回找到最后一个有值的点', () => {
    expect(lastIdx(mk([1, 2, 3, 4], [10, 20, null, null]))).toBe(1)
  })

  it('多条线里只要有一条有值就算数', () => {
    expect(lastIdx(mk([1, 2, 3], [10, null, null], [1, 2, 3]))).toBe(2)
  })

  it('整条线全是 null 时退回最后一个下标，不越界', () => {
    expect(lastIdx(mk([1, 2, 3], [null, null, null]))).toBe(2)
  })

  it('空数据不炸', () => {
    expect(lastIdx({ data: [[]] })).toBe(-1)
  })
})

describe('fmtTime', () => {
  it('没有值时给占位符而不是 NaN', () => {
    expect(fmtTime(null)).toBe('--')
  })

  it('秒级时间戳格式化成 时:分:秒，各位补零', () => {
    // 用本地时间构造，避免测试依赖运行环境的时区
    const d = new Date(2026, 0, 2, 3, 4, 5)
    expect(fmtTime(d.getTime() / 1000)).toBe('03:04:05')
  })
})

describe('fmtNum', () => {
  it('按量级给小数位，不把浮点噪声漏到图例上', () => {
    // 真实数据：内存 646.0238285064697 MiB —— 十几位小数没有意义
    expect(fmtNum(646.0238285064697)).toBe('646')
    expect(fmtNum(6.135355472564697)).toBe('6.14')
    expect(fmtNum(12.3456)).toBe('12.3')
  })

  it('空值和非数给占位符而不是 NaN', () => {
    expect(fmtNum(null)).toBe('—')
    expect(fmtNum(NaN)).toBe('—')
    expect(fmtNum(Infinity)).toBe('—')
  })

  it('负数按绝对值定位数', () => {
    expect(fmtNum(-646.02)).toBe('-646')
    expect(fmtNum(-1.234)).toBe('-1.23')
  })
})
