/** @vitest-environment happy-dom */

import { createApp } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { SettingsModel } from './desktop-api'
import { createSettingsFixture } from './test-fixtures/settings'

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn().mockResolvedValue(() => {}) }))
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }))
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn() }))
vi.mock('@tauri-apps/api/app', () => ({ setTheme: vi.fn().mockResolvedValue(undefined) }))
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ minimize: vi.fn(), toggleMaximize: vi.fn(), close: vi.fn(), isMaximized: vi.fn().mockResolvedValue(false), onResized: vi.fn().mockResolvedValue(() => {}) }),
}))

function settingsFixture(): SettingsModel {
  return createSettingsFixture({
    language: 'zh-CN',
    cloud_sync: { backend: 's3', remote_id: 'remote-a', vault_id: 'vault-a', generation_id: 'generation-a' },
  })
}

afterEach(() => {
  document.body.innerHTML = ''
  vi.doUnmock('./desktop-api')
  vi.resetModules()
})

describe('desktop locale recovery integration', () => {
  it('recovers persisted locale after the first settings read fails', async () => {
    const deferred = (() => { let resolve!: (value: SettingsModel) => void; const promise = new Promise<SettingsModel>((r) => { resolve = r }); return { promise, resolve } })()
    const setNativeLocale = vi.fn().mockResolvedValue(undefined)
    const api = {
      getSettings: vi.fn().mockRejectedValueOnce(new Error('settings unavailable')).mockReturnValueOnce(deferred.promise),
      setNativeLocale,
      searchSessions: vi.fn().mockResolvedValue({ sessions: [], total: 0, search_mode: 'hybrid', semantic_status: 'ready' }),
      getApiStatus: vi.fn().mockResolvedValue({ service: { state: 'starting' }, userscript_connected: false, mcp: { state: 'stopped' }, mcp_url: 'http://127.0.0.1:19821/mcp' }),
      getSemanticStatus: vi.fn().mockResolvedValue({ enabled: false, status: 'disabled', backend: 'local', model_id: '', pending_chunks: 0, ready_chunks: 0, local_model_ready: false }),
    }
    vi.doMock('./desktop-api', () => ({ desktopApi: api }))

    const [{ startDesktopApp }, { default: App }, { i18n, currentLocale }] = await Promise.all([
      import('./desktop-startup'), import('./App.vue'), import('./i18n'),
    ])
    const host = document.createElement('div')
    host.id = 'app'
    document.body.append(host)
    const app = await new Promise<ReturnType<typeof createApp> | null>((resolve) => {
      void startDesktopApp(api, (initialSettings) => {
        const instance = createApp(App, { initialSettings }).use(i18n)
        instance.mount(host)
        resolve(instance)
      }, ['en-US'])
    })
    expect(currentLocale()).toBe('en-US')
    expect(document.documentElement.lang).toBe('en-US')
    expect(setNativeLocale).toHaveBeenNthCalledWith(1, 'en-US')

    deferred.resolve(settingsFixture())
    await vi.waitFor(() => {
      expect(currentLocale()).toBe('zh-CN')
      expect(document.documentElement.lang).toBe('zh-CN')
      expect(document.body.textContent).toContain('全部对话')
      expect(setNativeLocale).toHaveBeenNthCalledWith(2, 'zh-CN')
    })
    app?.unmount()
  })
})

