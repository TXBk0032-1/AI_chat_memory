/** @vitest-environment happy-dom */

import { describe, expect, it, vi, beforeEach } from 'vitest'
import { ref } from 'vue'
import { useTheme } from './useTheme'
import type { SettingsModel } from '../desktop-api'

function createTestSettings(): SettingsModel {
  return {
    setup_complete: true,
    secret_enabled: false,
    allowed_origins: [],
    close_behavior: 'ask',
    tray_click_behavior: 'show_menu',
    theme: 'system',
    light_theme_id: 'green',
    dark_theme_id: 'black',
    language: 'system',
    semantic_search: {
      enabled: true,
      default_mode: 'hybrid',
      backend: 'local',
      local: { model: 'test', device: 'auto', dtype: 'auto' },
      ollama: { base_url: '', model: '' },
      llama_cpp: { base_url: '', model: '' },
      openai_compatible: { base_url: '', model: '' },
    },
    mcp_enabled: true,
    cloud_sync: {
      backend: 'webdav',
      enabled: false,
      connection_verified: false,
      base_url: '',
      root_path: '',
      username: '',
      encryption_enabled: false,
      s3: { endpoint_url: '', region: 'us-east-1', bucket: '', prefix: '', force_path_style: false },
      remote_id: 'default',
      vault_id: 'default',
      generation_id: 'gen-1',
    },
  }
}

describe('useTheme composable', () => {
  beforeEach(() => {
    document.documentElement.removeAttribute('data-theme')
    document.documentElement.removeAttribute('data-theme-id')
  })

  it('initializes and commits system theme', () => {
    const settings = ref(createTestSettings())
    const onApplied = vi.fn()
    const theme = useTheme(settings, onApplied)

    theme.initialize()
    expect(onApplied).toHaveBeenCalled()
    expect(document.documentElement.dataset.theme).toBeDefined()
    theme.dispose()
  })

  it('switches between light, dark and system themes', () => {
    const settings = ref(createTestSettings())
    const onApplied = vi.fn()
    const theme = useTheme(settings, onApplied)

    theme.previewTheme('light', 'blue', 'black')
    expect(settings.value.theme).toBe('light')
    expect(settings.value.light_theme_id).toBe('blue')
    expect(document.documentElement.dataset.theme).toBe('light')
    expect(document.documentElement.dataset.themeId).toBe('blue')

    theme.previewTheme('dark', 'blue', 'dark_blue')
    expect(settings.value.theme).toBe('dark')
    expect(settings.value.dark_theme_id).toBe('dark_blue')
    expect(document.documentElement.dataset.theme).toBe('dark')
    expect(document.documentElement.dataset.themeId).toBe('dark_blue')
  })

  it('supports previewThemeId for individual preset selection', () => {
    const settings = ref(createTestSettings())
    const onApplied = vi.fn()
    const theme = useTheme(settings, onApplied)

    theme.previewThemeId('orange', false)
    expect(settings.value.light_theme_id).toBe('orange')
    expect(settings.value.theme).toBe('light')
    expect(document.documentElement.dataset.themeId).toBe('orange')

    settings.value.theme = 'light'
    theme.previewThemeId('dark_purple', true)
    expect(settings.value.dark_theme_id).toBe('dark_purple')
    expect(settings.value.theme).toBe('dark')
    expect(document.documentElement.dataset.themeId).toBe('dark_purple')
  })

  it('handles preview begin, accept and cancel lifecycle', () => {
    const settings = ref(createTestSettings())
    settings.value.theme = 'light'
    settings.value.light_theme_id = 'green'
    const onApplied = vi.fn()
    const theme = useTheme(settings, onApplied)

    theme.beginPreview()
    theme.previewTheme('dark', 'green', 'dark_emerald')
    expect(settings.value.theme).toBe('dark')

    theme.cancelPreview()
    expect(settings.value.theme).toBe('light')
    expect(settings.value.light_theme_id).toBe('green')
    expect(document.documentElement.dataset.theme).toBe('light')
    expect(document.documentElement.dataset.themeId).toBe('green')
  })

  it('saves, activates and deletes custom themes', () => {
    const settings = ref(createTestSettings())
    const onApplied = vi.fn()
    const theme = useTheme(settings, onApplied)

    const customTheme = {
      id: 'custom_aurora',
      name: 'Aurora Dark',
      nameKey: '',
      isDark: true,
      isCustom: true,
      config: {
        primary: 'rgb(80, 200, 160)',
      },
    }

    theme.saveCustomTheme(customTheme, true)
    expect(settings.value.custom_themes).toHaveLength(1)
    expect(settings.value.dark_theme_id).toBe('custom_aurora')
    expect(settings.value.theme).toBe('dark')
    expect(document.documentElement.dataset.themeId).toBe('custom_aurora')

    theme.deleteCustomTheme('custom_aurora')
    expect(settings.value.custom_themes).toHaveLength(0)
    expect(settings.value.dark_theme_id).toBe('black')
  })
})
