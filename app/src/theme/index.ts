import { createThemeColors } from './generator'
import { findTheme, getDefaultTheme } from './presets'
import type { ResolvedTheme, ThemeDefinition, ThemeMode } from './types'

export * from './colorUtils'
export * from './generator'
export * from './presets'
export * from './types'

/**
 * Resolves the effective ThemeDefinition from user preference, custom theme IDs, and system dark mode state.
 */
export function resolveTheme(
  mode: ThemeMode = 'system',
  lightId?: string,
  darkId?: string,
  isSystemDark = false,
  customThemes: ThemeDefinition[] = [],
): ResolvedTheme {
  const isDark = mode === 'system' ? isSystemDark : mode === 'dark'
  const activeId = isDark ? (darkId || 'black') : (lightId || 'green')

  let themeDef: ThemeDefinition | undefined = findTheme(activeId, customThemes)
  if (!themeDef || themeDef.isDark !== isDark) {
    themeDef = getDefaultTheme(isDark, customThemes)
  }

  const colors = createThemeColors(
    themeDef.config.primary,
    themeDef.config.font,
    themeDef.isDark,
    themeDef.isDarkFont,
    themeDef.config.extInfo,
  )

  return {
    id: themeDef.id,
    name: themeDef.name,
    nameKey: themeDef.nameKey,
    isDark: themeDef.isDark,
    isDarkFont: Boolean(themeDef.isDarkFont),
    primary: themeDef.config.primary,
    colors,
  }
}

/**
 * Applies all computed CSS variables and data attributes of a resolved theme to the DOM.
 */
export function applyThemeToDOM(theme: ResolvedTheme): void {
  if (typeof document === 'undefined') return
  const root = document.documentElement
  const mode = theme.isDark ? 'dark' : 'light'

  root.dataset.theme = mode
  root.dataset.themeId = theme.id
  root.style.colorScheme = mode

  // Apply all generated variables to root
  for (const [key, value] of Object.entries(theme.colors)) {
    root.style.setProperty(key, value)
  }
}
