import { describe, expect, it, vi } from 'vitest'
import type { UnlistenFn } from '@tauri-apps/api/event'
import { runAppStartup } from './desktop-startup'
import { initializeAppSettings } from './app-settings-initialization'
import type { SettingsModel } from './desktop-api'

function settingsFixture(defaultMode: SettingsModel['semantic_search']['default_mode']): SettingsModel {
  return {
    setup_complete: true, secret_enabled: false, allowed_origins: [], close_behavior: 'ask', tray_click_behavior: 'show_menu', theme: 'system', language: 'system',
    semantic_search: { enabled: true, default_mode: defaultMode, backend: 'local', local: { model: 'test', device: 'auto', dtype: 'auto' }, ollama: { base_url: '', model: 'test' }, llama_cpp: { base_url: '', model: 'test' }, openai_compatible: { base_url: '', model: 'test' } },
    mcp_enabled: true,
    cloud_sync: { backend: 'webdav', enabled: false, connection_verified: false, base_url: '', root_path: '', username: '', encryption_enabled: false, s3: { endpoint_url: '', region: 'us-east-1', bucket: '', prefix: '', force_path_style: false }, remote_id: 'default', vault_id: 'default', generation_id: 'gen-1' },
  }
}

type Steps = Parameters<typeof runAppStartup>[0]

function stepOverrides(overrides: Partial<Steps>): Steps {
  return {
    settingsReady: Promise.resolve(),
    loadSessions: vi.fn(async () => {}),
    subscribeCloseBehavior: vi.fn().mockResolvedValue((() => {}) as UnlistenFn),
    refreshApiStatus: vi.fn(async () => {}),
    startStatusPolling: vi.fn(),
    ...overrides,
  }
}

describe('app startup pipeline (FE-6, FE-7)', () => {
  it('applies the persisted search mode before starting the first session load', async () => {
    const order: string[] = []
    let appliedMode = 'hybrid'
    const settings = settingsFixture('keyword')
    const settingsReady = initializeAppSettings({
      loadSettings: vi.fn(async () => { order.push('loadSettings'); return settings }),
      applyPreference: vi.fn(async () => 'zh-CN' as const),
      applySettings: vi.fn((value: SettingsModel) => {
        appliedMode = value.semantic_search.default_mode
        order.push('applySettings')
      }),
    })

    await runAppStartup(stepOverrides({
      settingsReady,
      loadSessions: async () => { order.push(`loadSessions:${appliedMode}`) },
    }))

    const applyIndex = order.indexOf('applySettings')
    const sessionsIndex = order.indexOf('loadSessions:keyword')
    expect(applyIndex).toBeGreaterThan(-1)
    expect(sessionsIndex).toBeGreaterThan(applyIndex)
    expect(order).toContain('loadSettings')
  })

  it('still loads the session list when loading settings fails', async () => {
    const loadSessions = vi.fn(async () => {})
    const settingsReady = initializeAppSettings({
      loadSettings: vi.fn().mockRejectedValue(new Error('settings down')),
      applyPreference: vi.fn(async () => 'zh-CN' as const),
      applySettings: vi.fn(),
    })

    const unlisten = await runAppStartup(stepOverrides({ settingsReady, loadSessions }))

    expect(loadSessions).toHaveBeenCalledTimes(1)
    expect(unlisten).toBeTypeOf('function')
  })

  it('keeps status polling and the session list running when the event subscription fails', async () => {
    const loadSessions = vi.fn(async () => {})
    const refreshApiStatus = vi.fn(async () => {})
    const startStatusPolling = vi.fn()
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {})

    const unlisten = await runAppStartup(stepOverrides({
      settingsReady: Promise.reject(new Error('settings down')),
      loadSessions,
      subscribeCloseBehavior: vi.fn().mockRejectedValue(new Error('event system down')),
      refreshApiStatus,
      startStatusPolling,
    }))

    expect(unlisten).toBeUndefined()
    expect(loadSessions).toHaveBeenCalledTimes(1)
    expect(refreshApiStatus).toHaveBeenCalledTimes(1)
    expect(startStatusPolling).toHaveBeenCalledTimes(1)
    errorSpy.mockRestore()
  })

  it('starts status polling after the initial refresh and settings settle', async () => {
    const order: string[] = []
    await runAppStartup(stepOverrides({
      refreshApiStatus: async () => { order.push('refreshApiStatus') },
      startStatusPolling: () => { order.push('startStatusPolling') },
    }))
    expect(order[0]).toBe('refreshApiStatus')
    expect(order[order.length - 1]).toBe('startStatusPolling')
  })
})
