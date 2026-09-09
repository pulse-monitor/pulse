import { describe, it, expect } from 'vitest'
import * as f from './format'

describe('bytes', () => {
  it('uses 1024 as the divisor, matching df -h', () => {
    expect(f.bytes(0)).toBe('0 B')
    expect(f.bytes(1023)).toBe('1023 B')
    expect(f.bytes(1024)).toBe('1.0 KiB')
    expect(f.bytes(1024 ** 3)).toBe('1.0 GiB')
    expect(f.bytes(245107195904)).toBe('228.3 GiB')
  })
  it('shows — for unknown instead of 0', () => {
    // 显示 0 B 会让用户以为「真的是 0」
    for (const v of [null, undefined, NaN, -1, Infinity]) {
      expect(f.bytes(v as number)).toBe('—')
    }
  })
})

describe('limit', () => {
  it('shows ♾️ when no limit is configured (R16)', () => {
    expect(f.limit(null)).toBe('♾️')
    expect(f.limit(undefined)).toBe('♾️')
    expect(f.limit(0)).toBe('♾️')
    expect(f.limit(1024 ** 4)).toBe('1.0 TiB')
  })
})

describe('duration', () => {
  it('picks the two most significant units', () => {
    expect(f.duration(0)).toBe('0分')
    expect(f.duration(90)).toBe('1分')
    expect(f.duration(3660)).toBe('1时1分')
    expect(f.duration(920146)).toBe('10天15时')
  })
  it('shows — for unknown', () => {
    expect(f.duration(null)).toBe('—')
    expect(f.duration(-1)).toBe('—')
  })
})

describe('remainDays', () => {
  it('distinguishes 买断 / 未设到期 / 已过期', () => {
    expect(f.remainDays(null, true)).toBe('♾️')
    // 未设到期时间显示 — 而不是「0 天」，后者会让人以为今天就到期
    expect(f.remainDays(null, false)).toBe('—')
    expect(f.remainDays(undefined)).toBe('—')
    expect(f.remainDays(200)).toBe('200 天')
    expect(f.remainDays(0)).toBe('0 天')
    expect(f.remainDays(-2)).toBe('已过期 2 天')
  })
})

describe('money', () => {
  it('shows — when the currency has no rate', () => {
    // 后端在无汇率时给 null；显示 0 会让总价看起来「少了一台」而无从察觉
    expect(f.money(null, 'CNY')).toBe('—')
  })
  it('uses zero decimals for JPY-like currencies', () => {
    expect(f.money(1234.56, 'CNY')).toBe('¥1234.56')
    expect(f.money(1234.56, 'JPY')).toBe('¥1235')
    expect(f.money(10.5, 'RUB')).toBe('10.50 RUB')
  })
})

describe('countryToFlag', () => {
  it('builds the flag from regional indicators', () => {
    expect(f.countryToFlag('US')).toBe('\u{1F1FA}\u{1F1F8}')
    expect(f.countryToFlag('cn')).toBe('\u{1F1E8}\u{1F1F3}')
    expect(f.countryToFlag('JP')).toBe('\u{1F1EF}\u{1F1F5}')
  })
  it('returns null for anything that is not a 2-letter code', () => {
    for (const v of [null, undefined, '', 'U', 'USA', '12', 'U1']) {
      expect(f.countryToFlag(v as string)).toBeNull()
    }
  })
})

describe('level', () => {
  it('turns yellow at 80 and red at 95', () => {
    expect(f.level(10)).toBe('ok')
    expect(f.level(79.9)).toBe('ok')
    expect(f.level(80)).toBe('warn')
    expect(f.level(94.9)).toBe('warn')
    expect(f.level(95)).toBe('danger')
    expect(f.level(null)).toBe('ok')
  })
})

describe('price / priceKind', () => {
  // 价格是用户定的三态约定，不是普通数字：-1 免费、0 不显示、>0 金额。
  // 后端 price_display 是照实换算的原始值，免费机器那里是个**负数**。
  // 真机上取到的实际数据就是 price: -1, price_display: -6.7105。
  const free = { price: -1, price_display: -6.7105 }
  const unset = { price: 0, price_display: 0 }
  const paid = { price: 5, price_display: 35.4 }

  it('免费机器显示「免费」，绝不能把负数漏出去', () => {
    expect(f.priceKind(free)).toBe('free')
    expect(f.price(free, 'CNY')).toBe('免费')
    expect(f.price(free, 'CNY')).not.toContain('-')
  })

  it('未填价格不显示', () => {
    expect(f.priceKind(unset)).toBe('hidden')
    expect(f.price(unset, 'CNY')).toBe('—')
  })

  it('正常价格照常换算', () => {
    expect(f.priceKind(paid)).toBe('amount')
    expect(f.price(paid, 'CNY')).toBe('¥35.40')
  })

  it('没有账单信息时不显示', () => {
    expect(f.priceKind(null)).toBe('hidden')
    expect(f.priceKind(undefined)).toBe('hidden')
    expect(f.price(null, 'CNY')).toBe('—')
  })

  it('换算失败（price_display 为 null）退回占位符，而不是 NaN', () => {
    expect(f.price({ price: 5, price_display: null }, 'CNY')).toBe('—')
  })
})
