import { setTheme as setNativeTheme } from '@tauri-apps/api/app'
import type { Ref } from 'vue'
import type { SettingsModel, ThemePreference } from '../desktop-api'

export function useTheme(settings: Ref<SettingsModel>, onApplied: (animate: boolean) => void) {
  let systemThemeQuery: MediaQueryList | undefined
  let savedPreference: ThemePreference = 'system'

  function effectiveTheme(preference = settings.value.theme) {
    return preference === 'system' ? (systemThemeQuery?.matches ? 'dark' : 'light') : preference
  }

  function commitTheme(preference: ThemePreference, animate = true) {
    const theme = effectiveTheme(preference)
    void setNativeTheme(theme).catch(() => {})
    if (document.documentElement.dataset.theme === theme) {
      document.documentElement.style.colorScheme = theme
      return
    }
    const apply = () => {
      document.documentElement.dataset.theme = theme
      document.documentElement.style.colorScheme = theme
    }
    const transitions = document as Document & { startViewTransition?: (callback: () => void) => void }
    if (animate && transitions.startViewTransition) transitions.startViewTransition(apply)
    else if (animate) {
      document.documentElement.classList.add('theme-transition')
      void document.documentElement.offsetWidth
      apply()
      window.setTimeout(() => document.documentElement.classList.remove('theme-transition'), 360)
    } else apply()
    onApplied(animate)
  }

  function previewTheme(theme: ThemePreference) {
    settings.value.theme = theme
    commitTheme(theme)
  }

  function beginPreview() {
    savedPreference = settings.value.theme
  }

  function acceptPreview() {
    savedPreference = settings.value.theme
  }

  function cancelPreview() {
    settings.value.theme = savedPreference
    commitTheme(savedPreference)
  }

  function handleSystemThemeChange() {
    if (settings.value.theme === 'system') commitTheme('system')
  }

  function initialize() {
    systemThemeQuery = window.matchMedia('(prefers-color-scheme: dark)')
    systemThemeQuery.addEventListener('change', handleSystemThemeChange)
    commitTheme('system', false)
  }

  function dispose() {
    systemThemeQuery?.removeEventListener('change', handleSystemThemeChange)
  }

  return { effectiveTheme, commitTheme, previewTheme, beginPreview, acceptPreview, cancelPreview, initialize, dispose }
}
