import { describe, expect, it, vi } from 'vitest'
import type { SettingsModel } from './desktop-api'
import { initializeAppSettings } from './app-settings-initialization'
import { createSettingsFixture } from './test-fixtures/settings'

function settingsFixture(language: SettingsModel['language']): SettingsModel {
  return createSettingsFixture({
    language,
    cloud_sync: {
      backend: 's3',
      s3: { bucket: 'archive' },
      remote_id: 'remote-a',
      vault_id: 'vault-a',
      generation_id: 'generation-a',
    },
  })
}

describe('app settings initialization', () => {
  it('reuses preloaded settings without loading or syncing locale twice', async () => {
    const value = settingsFixture('en-US')
    const loadSettings = vi.fn()
    const applyPreference = vi.fn()
    const applySettings = vi.fn()
    await initializeAppSettings({ initialSettings: value, loadSettings, applyPreference, applySettings })
    expect(loadSettings).not.toHaveBeenCalled()
    expect(applyPreference).not.toHaveBeenCalled()
    expect(applySettings).toHaveBeenCalledWith(value)
  })

  it('applies the persisted locale before installing settings after startup retry', async () => {
    const order: string[] = []
    const value = settingsFixture('zh-CN')
    await initializeAppSettings({
      loadSettings: vi.fn().mockResolvedValue(value),
      applyPreference: vi.fn(async (language) => { order.push(`locale:${language}`); return 'zh-CN' as const }),
      applySettings: vi.fn(() => order.push('settings')),
    })
    expect(order).toEqual(['locale:zh-CN', 'settings'])
  })

  it('installs settings even when native locale synchronization fails', async () => {
    const value = settingsFixture('en-US')
    const applySettings = vi.fn()
    await expect(initializeAppSettings({ loadSettings: vi.fn().mockResolvedValue(value), applyPreference: vi.fn().mockRejectedValue(new Error('native')), applySettings })).rejects.toThrow('native')
    expect(applySettings).toHaveBeenCalledWith(value)
  })
})

