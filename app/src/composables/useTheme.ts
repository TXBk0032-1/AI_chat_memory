import { setTheme as setNativeTheme } from '@tauri-apps/api/app'
import { toRaw } from 'vue'
import type { Ref } from 'vue'
import type { SettingsModel, ThemePreference } from '../desktop-api'
import { applyThemeToDOM, resolveTheme, type ResolvedTheme, type ThemeDefinition } from '../theme'

export function useTheme(settings: Ref<SettingsModel>, onApplied: (animate: boolean) => void) {
  let systemThemeQuery: MediaQueryList | undefined
  let savedPreference: ThemePreference = 'system'
  let savedLightId: string | undefined
  let savedDarkId: string | undefined
  let savedCustomThemes: ThemeDefinition[] = []
  let themeTransitionTimer: number | undefined

  // The preview snapshot must cover custom_themes too: creating, editing or
  // deleting a theme mutates the live settings object, and a dialog cancel
  // would otherwise leave those edits permanently applied.
  function snapshotCustomThemes() {
    const raw = toRaw(settings.value).custom_themes || []
    savedCustomThemes = structuredClone(raw) as ThemeDefinition[]
  }

  function effectiveTheme(preference = settings.value.theme): 'light' | 'dark' {
    return preference === 'system' ? (systemThemeQuery?.matches ? 'dark' : 'light') : preference
  }

  function resolveCurrentTheme(
    preference = settings.value.theme,
    lightId = settings.value.light_theme_id,
    darkId = settings.value.dark_theme_id,
  ): ResolvedTheme {
    const isSystemDark = Boolean(
      systemThemeQuery?.matches ??
        (typeof window !== 'undefined' && typeof window.matchMedia === 'function'
          ? window.matchMedia('(prefers-color-scheme: dark)').matches
          : false),
    )
    return resolveTheme(preference, lightId, darkId, isSystemDark, settings.value.custom_themes || [])
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
      // Track the cleanup timer so rapid consecutive switches reschedule the
      // removal instead of stacking timers that cut the transition short.
      if (themeTransitionTimer !== undefined) window.clearTimeout(themeTransitionTimer)
      themeTransitionTimer = window.setTimeout(() => {
        themeTransitionTimer = undefined
        document.documentElement.classList.remove('theme-transition')
      }, 360)
    } else {
      apply()
    }
    onApplied(animate)
  }

  function previewTheme(theme: ThemePreference, lightId?: string, darkId?: string) {
    settings.value.theme = theme
    if (lightId !== undefined) settings.value.light_theme_id = lightId
    if (darkId !== undefined) settings.value.dark_theme_id = darkId
    commitTheme(theme, settings.value.light_theme_id, settings.value.dark_theme_id, true)
  }

  function previewThemeId(id: string, isDark: boolean) {
    if (isDark) {
      settings.value.dark_theme_id = id
      if (settings.value.theme !== 'dark') {
        settings.value.theme = 'dark'
      }
    } else {
      settings.value.light_theme_id = id
      if (settings.value.theme !== 'light') {
        settings.value.theme = 'light'
      }
    }
    commitTheme(settings.value.theme, settings.value.light_theme_id, settings.value.dark_theme_id, true)
  }

  function saveCustomTheme(theme: ThemeDefinition, activate = false) {
    const list = [...(settings.value.custom_themes || [])]
    const idx = list.findIndex((t) => t.id === theme.id)
    if (idx >= 0) {
      list[idx] = { ...theme, isCustom: true }
    } else {
      list.push({ ...theme, isCustom: true })
    }
    settings.value.custom_themes = list

    if (activate) {
      previewThemeId(theme.id, theme.isDark)
    } else {
      const activeId = theme.isDark ? settings.value.dark_theme_id : settings.value.light_theme_id
      if (activeId === theme.id) {
        commitTheme(settings.value.theme, settings.value.light_theme_id, settings.value.dark_theme_id, true)
      }
    }
  }

  function deleteCustomTheme(id: string) {
    const list = (settings.value.custom_themes || []).filter((t) => t.id !== id)
    settings.value.custom_themes = list

    if (settings.value.light_theme_id === id) {
      settings.value.light_theme_id = 'green'
    }
    if (settings.value.dark_theme_id === id) {
      settings.value.dark_theme_id = 'black'
    }
    commitTheme(settings.value.theme, settings.value.light_theme_id, settings.value.dark_theme_id, true)
  }

  function beginPreview() {
    savedPreference = settings.value.theme
    savedLightId = settings.value.light_theme_id
    savedDarkId = settings.value.dark_theme_id
    snapshotCustomThemes()
  }

  function acceptPreview() {
    savedPreference = settings.value.theme
    savedLightId = settings.value.light_theme_id
    savedDarkId = settings.value.dark_theme_id
    snapshotCustomThemes()
  }

  function cancelPreview() {
    settings.value.theme = savedPreference
    settings.value.light_theme_id = savedLightId
    settings.value.dark_theme_id = savedDarkId
    settings.value.custom_themes = savedCustomThemes
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
    if (themeTransitionTimer !== undefined) {
      window.clearTimeout(themeTransitionTimer)
      themeTransitionTimer = undefined
    }
    document.documentElement.classList.remove('theme-transition')
  }

  return {
    effectiveTheme,
    resolveCurrentTheme,
    commitTheme,
    previewTheme,
    previewThemeId,
    saveCustomTheme,
    deleteCustomTheme,
    beginPreview,
    acceptPreview,
    cancelPreview,
    initialize,
    dispose,
  }
}
