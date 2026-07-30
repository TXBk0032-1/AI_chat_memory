/** @vitest-environment happy-dom */

import { describe, expect, it, vi } from 'vitest'
import { initializeLocaleAndMount } from './index'

describe('startup locale initialization', () => {
  it('applies the saved language and syncs native UI before mount', async () => {
    const api = {
      getSettings: vi.fn().mockResolvedValue({ language: 'zh-CN' }),
      setNativeLocale: vi.fn().mockResolvedValue(undefined),
    }
    const mount = vi.fn()

    const locale = await initializeLocaleAndMount(api, mount, ['en-US'])

    expect(locale).toBe('zh-CN')
    expect(document.documentElement.lang).toBe('zh-CN')
    expect(api.setNativeLocale).toHaveBeenCalledWith('zh-CN')
    expect(mount).toHaveBeenCalledWith({ language: 'zh-CN' })
  })

  it('falls back to the system language and continues when settings cannot be read', async () => {
    const api = {
      getSettings: vi.fn().mockRejectedValue(new Error('settings unavailable')),
      setNativeLocale: vi.fn().mockResolvedValue(undefined),
    }
    const mount = vi.fn()

    const locale = await initializeLocaleAndMount(api, mount, ['en-GB'])

    expect(locale).toBe('en-US')
    expect(document.documentElement.lang).toBe('en-US')
    expect(api.setNativeLocale).toHaveBeenCalledWith('en-US')
    expect(mount).toHaveBeenCalledWith(undefined)
  })
})


