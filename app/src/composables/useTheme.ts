import { setTheme as setNativeTheme } from '@tauri-apps/api/app'
import type { Ref } from 'vue'
import type { SettingsModel, ThemePreference } from '../desktop-api'
import { applyThemeToDOM, resolveTheme, type ResolvedTheme } from '../theme'

export function useTheme(settings: Ref<SettingsModel>, onApplied: (animate: boolean) => void) {
  let systemThemeQuery: MediaQueryList | undefined
  let savedPreference: ThemePreference = 'system'
  let savedLightId: string | undefined
  let savedDarkId: string | undefined

  function effectiveTheme(preference = settings.value.theme): 'light' | 'dark' {
    return preference === 'system' ? (systemThemeQuery?.matches ? 'dark' : 'light') : preference
  }

  function resolveCurrentTheme(
    preference = settings.value.theme,
    lightId = settings.value.light_theme_id,
    darkId = settings.value.dark_theme_id,
  ): ResolvedTheme {
    const isSystemDark = Boolean(systemThemeQuery?.matches)
    return resolveTheme(preference, lightId, darkId, isSystemDark)
  }

  function commitTheme(
    preference: ThemePreference = settings.value.theme,
    lightId = settings.value.light_theme_id,
    darkId = settings.value.dark_theme_id,
    animate = true,
  ) {
    const resolved = resolveCurrentTheme(preference, lightId, darkId)
    const effectiveMode = resolved.isDark ? 'dark' : 'light'

    void setNativeTheme(effectiveMode).catch(() => {})

    const apply = () => {
      applyThemeToDOM(resolved)
    }

    const transitions = document as Document & { startViewTransition?: (callback: () => void) => void }
    if (animate && transitions.startViewTransition) {
      transitions.startViewTransition(apply)
    } else if (animate) {
      document.documentElement.classList.add('theme-transition')
      void document.documentElement.offsetWidth
      apply()
      window.setTimeout(() => document.documentElement.classList.remove('theme-transition'), 360)
    } else {
      apply()
    }
    onApplied(animate)
  }

  function previewTheme(theme: ThemePreference, lightId?: string, darkId?: string) {
    settings.value.theme = theme
    if (lightId !== undefined) settings.value.light_theme_id = lightId
    if (darkId !== undefined) settings.value.dark_theme_id = darkId
    commitTheme(theme, lightId, darkId, true)
  }

  function previewThemeId(id: string, isDark: boolean) {
    if (isDark) {
      settings.value.dark_theme_id = id
      if (settings.value.theme === 'light') {
        settings.value.theme = 'dark'
      }
    } else {
      settings.value.light_theme_id = id
      if (settings.value.theme === 'dark') {
        settings.value.theme = 'light'
      }
    }
    commitTheme(settings.value.theme, settings.value.light_theme_id, settings.value.dark_theme_id, true)
  }

  function beginPreview() {
    savedPreference = settings.value.theme
    savedLightId = settings.value.light_theme_id
    savedDarkId = settings.value.dark_theme_id
  }

  function acceptPreview() {
    savedPreference = settings.value.theme
    savedLightId = settings.value.light_theme_id
    savedDarkId = settings.value.dark_theme_id
  }

  function cancelPreview() {
    settings.value.theme = savedPreference
    settings.value.light_theme_id = savedLightId
    settings.value.dark_theme_id = savedDarkId
    commitTheme(savedPreference, savedLightId, savedDarkId, true)
  }

  function handleSystemThemeChange() {
    if (settings.value.theme === 'system') {
      commitTheme('system', settings.value.light_theme_id, settings.value.dark_theme_id, true)
    }
  }

  function initialize() {
    systemThemeQuery = window.matchMedia('(prefers-color-scheme: dark)')
    systemThemeQuery.addEventListener('change', handleSystemThemeChange)
    commitTheme('system', settings.value.light_theme_id, settings.value.dark_theme_id, false)
  }

  function dispose() {
    systemThemeQuery?.removeEventListener('change', handleSystemThemeChange)
  }

  return {
    effectiveTheme,
    resolveCurrentTheme,
    commitTheme,
    previewTheme,
    previewThemeId,
    beginPreview,
    acceptPreview,
    cancelPreview,
    initialize,
    dispose,
  }
}
