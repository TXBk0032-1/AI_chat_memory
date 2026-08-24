/** @vitest-environment happy-dom */

import { describe, expect, it } from 'vitest'
import {
  allThemes,
  darkThemes,
  findTheme,
  getDefaultTheme,
  lightThemes,
  duplicateThemeAsCustom,
  createDefaultCustomTheme,
} from './presets'
import { applyThemeToDOM, resolveTheme } from './index'

describe('theme presets and index', () => {
  it('contains populated light and dark theme definitions', () => {
    expect(lightThemes.length).toBeGreaterThanOrEqual(10)
    expect(darkThemes.length).toBeGreaterThanOrEqual(5)
    expect(allThemes.length).toBe(lightThemes.length + darkThemes.length)

    for (const theme of allThemes) {
      expect(theme.id).toBeTruthy()
      expect(theme.name).toBeTruthy()
      expect(theme.nameKey).toBeTruthy()
      expect(theme.config.primary).toBeTruthy()
    }
  })

  it('finds themes by id and resolves defaults', () => {
    const green = findTheme('green')
    expect(green).toBeDefined()
    expect(green?.isDark).toBe(false)

    const black = findTheme('black')
    expect(black).toBeDefined()
    expect(black?.isDark).toBe(true)

    expect(getDefaultTheme(false).id).toBe('green')
    expect(getDefaultTheme(true).id).toBe('black')
  })

  it('resolves effective theme based on mode and system preference', () => {
    const lightResolved = resolveTheme('light', 'blue', 'black', false)
    expect(lightResolved.id).toBe('blue')
    expect(lightResolved.isDark).toBe(false)

    const darkResolved = resolveTheme('dark', 'blue', 'dark_blue', false)
    expect(darkResolved.id).toBe('dark_blue')
    expect(darkResolved.isDark).toBe(true)

    const systemLightResolved = resolveTheme('system', 'orange', 'black', false)
    expect(systemLightResolved.id).toBe('orange')
    expect(systemLightResolved.isDark).toBe(false)

    const systemDarkResolved = resolveTheme('system', 'orange', 'dark_purple', true)
    expect(systemDarkResolved.id).toBe('dark_purple')
    expect(systemDarkResolved.isDark).toBe(true)
  })

  it('supports custom themes creation, duplication, and resolution', () => {
    const customLight = {
      id: 'custom_light_1',
      name: 'My Custom Light',
      nameKey: '',
      isDark: false,
      isCustom: true,
      config: {
        primary: 'rgb(100, 150, 200)',
      },
    }

    const found = findTheme('custom_light_1', [customLight])
    expect(found).toBeDefined()
    expect(found?.name).toBe('My Custom Light')

    const resolved = resolveTheme('light', 'custom_light_1', 'black', false, [customLight])
    expect(resolved.id).toBe('custom_light_1')
    expect(resolved.colors['--color-primary']).toBe('rgb(100, 150, 200)')

    const duplicated = duplicateThemeAsCustom(customLight, 'Cloned Theme')
    expect(duplicated.id).not.toBe(customLight.id)
    expect(duplicated.name).toBe('Cloned Theme')
    expect(duplicated.isCustom).toBe(true)

    const created = createDefaultCustomTheme(true, 'Brand New Dark')
    expect(created.isDark).toBe(true)
    expect(created.name).toBe('Brand New Dark')
  })

  it('applies resolved theme to DOM', () => {
    const resolved = resolveTheme('light', 'green', 'black', false)
    applyThemeToDOM(resolved)
    expect(document.documentElement.dataset.theme).toBe('light')
    expect(document.documentElement.dataset.themeId).toBe('green')
    expect(document.documentElement.style.getPropertyValue('--color-primary')).toBeTruthy()
  })
})
