import { describe, expect, it } from 'vitest'
import { detectOs } from './OsIcon'

describe('detectOs', () => {
  it('认得出常见发行版', () => {
    expect(detectOs('Linux (Debian GNU/Linux 13)')).toBe('debian')
    expect(detectOs('Ubuntu 24.04.1 LTS')).toBe('ubuntu')
    expect(detectOs('Alpine Linux v3.20')).toBe('alpine')
    expect(detectOs('CentOS Stream 9')).toBe('centos')
    expect(detectOs('Rocky Linux 9.4')).toBe('rocky')
    expect(detectOs('macOS 26.5.2')).toBe('macos')
    expect(detectOs('Windows Server 2022')).toBe('windows')
  })

  it('更具体的发行版优先于通用 Linux', () => {
    // 这些串里都带 "Linux"，不能被兜底规则先吃掉
    expect(detectOs('Ubuntu 22.04 Linux')).toBe('ubuntu')
    expect(detectOs('Debian GNU/Linux 12')).toBe('debian')
    expect(detectOs('Alpine Linux')).toBe('alpine')
  })

  it('认不出发行版但认得出是 Linux 时给通用图标', () => {
    expect(detectOs('Linux 6.1.0')).toBe('linux')
  })

  it('完全认不出时返回 null（整个图标不显示，不画问号占位）', () => {
    expect(detectOs(null)).toBeNull()
    expect(detectOs(undefined)).toBeNull()
    expect(detectOs('')).toBeNull()
    expect(detectOs('SomeUnknownOS 1.0')).toBeNull()
  })

  it('大小写不敏感', () => {
    expect(detectOs('DEBIAN 12')).toBe('debian')
    expect(detectOs('windows 11')).toBe('windows')
  })
})
