import { describe, it, expect } from 'vitest'
import { matchStatus } from './Home'
import type { ServerEntry } from '../api'

/** 只造 `matchStatus` 会看的字段，其余用 as 断言绕过 —— 这里测的是筛选逻辑。 */
function mk(p: {
  online?: boolean
  cpu?: number
  memPct?: number
  memTotal?: number
  remain?: number | null
  infinite?: boolean
  unknown?: boolean
  billing?: boolean
}): ServerEntry {
  return {
    server: {
      online: p.online ?? true,
      cpu_pct: p.cpu ?? 0,
      mem: { pct: p.memPct ?? 0, total: p.memTotal ?? 1024, used: 0 },
    },
    billing:
      p.billing === false
        ? null
        : {
            infinite: p.infinite ?? false,
            unknown_expiry: p.unknown ?? false,
            remain_days: p.remain === undefined ? 100 : p.remain,
          },
  } as unknown as ServerEntry
}

describe('matchStatus', () => {
  it('全部：什么都留下', () => {
    expect(matchStatus(mk({ online: false }), 'all')).toBe(true)
  })

  it('离线：只看在线标志', () => {
    expect(matchStatus(mk({ online: false }), 'offline')).toBe(true)
    expect(matchStatus(mk({ online: true }), 'offline')).toBe(false)
  })

  describe('高负载', () => {
    it('CPU 或内存任一过 80% 就算', () => {
      expect(matchStatus(mk({ cpu: 80 }), 'busy')).toBe(true)
      expect(matchStatus(mk({ memPct: 95 }), 'busy')).toBe(true)
      expect(matchStatus(mk({ cpu: 79.9, memPct: 79.9 }), 'busy')).toBe(false)
    })

    // 这一条是重点：离线机器留着的是**断线前**那一帧数据。
    // 不排除的话，一台在 90% 负载上崩掉的机器会永远挂在「高负载」里，
    // 而它其实已经躺平了 —— 该出现在「离线」而不是「高负载」。
    it('离线的机器不算高负载，哪怕它最后一帧是 99%', () => {
      expect(matchStatus(mk({ online: false, cpu: 99, memPct: 99 }), 'busy')).toBe(false)
    })

    it('内存总量为 0（采不到）时不拿内存判定', () => {
      expect(matchStatus(mk({ memTotal: 0, memPct: 100 }), 'busy')).toBe(false)
    })
  })

  describe('即将到期', () => {
    it('7 天内算，含已过期', () => {
      expect(matchStatus(mk({ remain: 7 }), 'expiring')).toBe(true)
      expect(matchStatus(mk({ remain: 0 }), 'expiring')).toBe(true)
      expect(matchStatus(mk({ remain: -3 }), 'expiring')).toBe(true)
      expect(matchStatus(mk({ remain: 8 }), 'expiring')).toBe(false)
    })

    it('长期 / 没设到期 / 没账单信息，都不算', () => {
      expect(matchStatus(mk({ infinite: true, remain: 1 }), 'expiring')).toBe(false)
      expect(matchStatus(mk({ unknown: true, remain: 1 }), 'expiring')).toBe(false)
      expect(matchStatus(mk({ remain: null }), 'expiring')).toBe(false)
      expect(matchStatus(mk({ billing: false }), 'expiring')).toBe(false)
    })

    it('离线但快到期的机器仍然要提示 —— 到期和在不在线无关', () => {
      expect(matchStatus(mk({ online: false, remain: 2 }), 'expiring')).toBe(true)
    })
  })
})
