/** @vitest-environment happy-dom */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createApp, defineComponent, h, nextTick } from 'vue'
import SettingsDialog from './SettingsDialog.vue'
import { setLocale } from '../i18n'
import type { SettingsModel } from '../desktop-api'

function baseSettings(): SettingsModel {
  return {
    setup_complete: true,
    secret_enabled: false,
    allowed_origins: [],
    close_behavior: 'ask',
    tray_click_behavior: 'show_menu',
    theme: 'system',
    language: 'zh-CN',
    mcp_enabled: false,
    codex: { auto_watch: false },
    semantic_search: {
      enabled: false,
      default_mode: 'hybrid',
      backend: 'local',
      local: { model: 'test-model', device: 'auto', dtype: 'auto' },
      ollama: { base_url: '', model: '' },
      llama_cpp: { base_url: '', model: '' },
      openai_compatible: { base_url: '', model: '' },
    },
    cloud_sync: { backend: 'webdav', enabled: false, connection_verified: false, base_url: '', root_path: '', username: '', encryption_enabled: false, s3: { endpoint_url: '', region: 'us-east-1', bucket: '', prefix: '', force_path_style: false }, remote_id: 'default', vault_id: 'default', generation_id: 'generation-1' },
  } as SettingsModel
}

describe('SettingsDialog codex import', () => {
  setLocale('zh-CN')
  afterEach(() => { document.body.innerHTML = '' })

  it('emits importCodex when the import button is clicked', async () => {
    document.body.innerHTML = '<div id="app"></div>'
    const onImport = vi.fn()
    const Root = defineComponent({ setup: () => () => h(SettingsDialog, { visible: true, secretCopied: false, settings: baseSettings(), originText: '', onImportCodex: onImport }) })
    const app = createApp(Root)
    app.mount(document.getElementById('app')!)
    try {
      await nextTick()
      const button = document.querySelector('.codex-import-now') as HTMLButtonElement
      expect(button).toBeTruthy()
      button.click()
      expect(onImport).toHaveBeenCalled()
    } finally { app.unmount() }
  })
})
