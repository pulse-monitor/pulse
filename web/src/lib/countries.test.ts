import { describe, it, expect } from 'vitest'
import { latlon, COUNTRY_LATLON } from './countries'

describe('latlon', () => {
  it('prefers explicit coordinates over the country centroid', () => {
    // 后台手填的坐标比国家中心准得多（VPS 网段的 GeoIP 经常不准）
    expect(latlon(34.05, -118.24, 'US')).toEqual([34.05, -118.24])
  })
  it('falls back to the country centroid', () => {
    expect(latlon(null, null, 'JP')).toEqual(COUNTRY_LATLON.JP)
    expect(latlon(null, null, 'jp')).toEqual(COUNTRY_LATLON.JP)
  })
  it('returns null when nothing is known, so the marker is skipped', () => {
    // 打在 (0,0) 会让所有未知位置的机器堆在几内亚湾
    expect(latlon(null, null, null)).toBeNull()
    expect(latlon(null, null, 'ZZ')).toBeNull()
  })
  it('ignores non-finite coordinates', () => {
    expect(latlon(NaN, 1, 'JP')).toEqual(COUNTRY_LATLON.JP)
    expect(latlon(1, Infinity, null)).toBeNull()
  })
  it('has plausible coordinates for every entry', () => {
    for (const [code, [la, lo]] of Object.entries(COUNTRY_LATLON)) {
      expect(code).toMatch(/^[A-Z]{2}$/)
      expect(la).toBeGreaterThanOrEqual(-90)
      expect(la).toBeLessThanOrEqual(90)
      expect(lo).toBeGreaterThanOrEqual(-180)
      expect(lo).toBeLessThanOrEqual(180)
    }
  })
})
