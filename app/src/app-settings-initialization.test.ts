import { describe, expect, it, vi } from 'vitest'
import type { SettingsModel } from './desktop-api'
import { initializeAppSettings } from './app-settings-initialization'

function settingsFixture(language: SettingsModel['language']): SettingsModel {
  return {
    setup_complete: true, secret_enabled: false, allowed_origins: [], close_behavior: 'ask', tray_click_behavior: 'show_menu', theme: 'system', language,
    semantic_search: { enabled: true, default_mode: 'hybrid', backend: 'local', local: { model: 'test', device: 'auto', dtype: 'auto' }, ollama: { base_url: '', model: 'test' }, llama_cpp: { base_url: '', model: 'test' }, openai_compatible: { base_url: '', model: 'test' } },
    mcp_enabled: true,
    cloud_sync: { backend: 's3', enabled: false, connection_verified: false, base_url: '', root_path: '', username: '', encryption_enabled: false, s3: { endpoint_url: '', region: 'us-east-1', bucket: 'archive', prefix: '', force_path_style: false }, remote_id: 'remote-a', vault_id: 'vault-a', generation_id: 'generation-a' },
  }
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

