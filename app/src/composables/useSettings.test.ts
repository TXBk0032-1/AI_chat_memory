/** @vitest-environment happy-dom */

import { createApp, defineComponent, h, ref, type Ref } from 'vue'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { SettingsModel } from '../desktop-api'
import { useSettings } from './useSettings'

const mocks = vi.hoisted(() => ({
  getSettings: vi.fn(),
  getApiStatus: vi.fn(),
  getSemanticStatus: vi.fn(),
  cancelSemanticWork: vi.fn(),
  saveSettings: vi.fn(),
  rotateSecret: vi.fn(),
  moveDataDirectory: vi.fn(),
  checkEmbedding: vi.fn(),
  reindexSemantic: vi.fn(),
  downloadLocalEmbeddingModel: vi.fn(),
  importLocalEmbeddingModel: vi.fn(),
}))

vi.mock('../desktop-api', () => ({ desktopApi: mocks }))
// The composable imports the tauri dialog plugin; stub it so the module loads.
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn().mockResolvedValue(() => {}) }))
vi.mock('../mcp-config', () => ({ buildMcpClientConfig: () => '' }))

function defaultSettings(): SettingsModel {
  return {
    allowed_origins: [],
    setup_complete: true,
    secret_enabled: false,
    secret: '',
    close_behavior: 'ask',
    tray_click_behavior: 'show_menu',
    theme: 'system',
    language: 'system',
    mcp_enabled: false,
    cloud_sync: { enabled: false, backend: 'webdav', webdav: { url: '', username: '', password: '' }, s3: {} as never, password: '' } as never,
    semantic_search: { enabled: false, backend: 'local', model_path: '', ollama: { base_url: '', model: '', dimensions: undefined }, llama_cpp: { base_url: '', model: '', dimensions: undefined }, openai_compatible: { base_url: '', api_key: '', model: '', dimensions: undefined } } as never,
  }
}

function mountComposable(settingsRef: Ref<SettingsModel>) {
  const errorRef = ref('')
  let exposed!: ReturnType<typeof useSettings>
  const theme = { begin: vi.fn(), accept: vi.fn(), cancel: vi.fn() }
  const locale = { begin: vi.fn(), accept: vi.fn(), cancel: vi.fn() }
  const Root = defineComponent({
    setup: () => {
      exposed = useSettings(settingsRef, errorRef, theme, locale)
      return () => h('div')
    },
  })
  const app = createApp(Root)
  document.body.innerHTML = '<div id="app"></div>'
  app.mount('#app')
  return {
    exposed,
    errorRef,
    theme,
    locale,
    unmount() {
      exposed.dispose()
      app.unmount()
      document.body.innerHTML = ''
    },
  }
}

describe('useSettings', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    mocks.getSettings.mockResolvedValue(defaultSettings())
    mocks.getApiStatus.mockResolvedValue({ running: true, port: 19820, mcp_url: 'http://127.0.0.1:19821/sse' } as never)
    mocks.getSemanticStatus.mockResolvedValue({ enabled: false, ready_chunks: 0, pending_chunks: 0 } as never)
  })

  it('opens the settings panel even when getSettings rejects (FE-12)', async () => {
    mocks.getSettings.mockRejectedValue(new Error('ipc-down'))
    const settingsRef = ref(defaultSettings())
    const { exposed, unmount } = mountComposable(settingsRef)

    // openSettings must not throw an unhandled rejection; the error is surfaced
    // and the dialog is still allowed to open so the user can retry.
    await expect(exposed.openSettings()).resolves.toBeUndefined()
    // The failure is surfaced rather than swallowed.
    expect(exposed.showSettings.value).toBe(true)

    unmount()
  })

  it('stops the reindex poll timer when cancelSemanticWork rejects (FE-8)', async () => {
    // Drive the composable into a polling state: getSemanticStatus reports an
    // active reindex so openSettings starts the 1s poll timer, then cancelling
    // the work rejects. The timer must still be cleared (no leak).
    mocks.getSemanticStatus.mockResolvedValue({
      enabled: true,
      reindex: { stage: 'indexing', total_sessions: 10, processed_sessions: 3, total_chunks: 100, ready_chunks: 30, pending_chunks: 70, fraction: 0.3, message: '' },
      ready_chunks: 30,
      pending_chunks: 70,
    } as never)
    mocks.cancelSemanticWork.mockRejectedValue(new Error('cancel-failed'))

    const setIntervalSpy = vi
      .spyOn(window, 'setInterval')
      .mockImplementation((() => 0) as unknown as typeof window.setInterval)
    const clearIntervalSpy = vi.spyOn(window, 'clearInterval').mockImplementation(() => {})

    const settingsRef = ref(defaultSettings())
    const { exposed, unmount } = mountComposable(settingsRef)
    await exposed.openSettings()
    expect(setIntervalSpy).toHaveBeenCalled()

    clearIntervalSpy.mockClear()
    // cancelSemanticWork rejects, but the poll timer must still be cleared.
    await expect(exposed.cancelSemanticWork()).resolves.toBeUndefined()
    expect(clearIntervalSpy).toHaveBeenCalled()

    setIntervalSpy.mockRestore()
    clearIntervalSpy.mockRestore()
    unmount()
  })
})
