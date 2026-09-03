import {
  RGB_Alpha_Shade,
  RGB_Linear_Shade,
  normalizeColor,
} from './colorUtils'
import type { ThemeExtInfo } from './types'

/**
 * Generates the full palette of CSS color variables from a primary color, font color, and dark flag.
 * Mirrors the LX Music algorithm while augmenting with semantic UI tokens.
 */
export function createThemeColors(
  primaryColor: string,
  fontColor?: string,
  isDark = false,
  isDarkFont = false,
  extInfo?: ThemeExtInfo,
): Record<string, string> {
  const normPrimary = normalizeColor(primaryColor)
  const colors: Record<string, string> = {
    '--color-primary': normPrimary,
  }

  // Dark shade steps (100 - 1000)
  let preColor = normPrimary
  for (let i = 1; i <= 10; i += 1) {
    preColor = RGB_Linear_Shade(isDark ? 0.2 : -0.1, preColor)
    colors[`--color-primary-dark-${i * 100}`] = preColor
    for (let j = 1; j < 10; j += 1) {
      colors[`--color-primary-dark-${i * 100}-alpha-${j * 100}`] = RGB_Alpha_Shade(0.1 * j, preColor)
      colors[`--color-primary-alpha-${j * 100}`] = RGB_Alpha_Shade(0.1 * j, normPrimary)
    }
  }

  // Light shade steps (100 - 1000)
  preColor = normPrimary
  for (let i = 1; i < 10; i += 1) {
    preColor = RGB_Linear_Shade(isDark ? -0.1 : 0.2, preColor)
    colors[`--color-primary-light-${i * 100}`] = preColor
    for (let j = 1; j < 10; j += 1) {
      colors[`--color-primary-light-${i * 100}-alpha-${j * 100}`] = RGB_Alpha_Shade(0.1 * j, preColor)
    }
  }

  preColor = RGB_Linear_Shade(isDark ? -0.35 : 1, preColor)
  colors['--color-primary-light-1000'] = preColor
  for (let j = 1; j < 10; j += 1) {
    colors[`--color-primary-light-1000-alpha-${j * 100}`] = RGB_Alpha_Shade(0.1 * j, preColor)
  }

  colors['--color-theme'] = isDark ? colors['--color-primary-light-900'] : normPrimary

  // Font color scale
  const fontColors = createFontColors(fontColor, isDark, isDarkFont)
  Object.assign(colors, fontColors)

  // Derived Semantic Tokens
  if (isDark) {
    colors['--color-primary-hover'] = colors['--color-primary-light-200'] || normPrimary
    colors['--color-primary-active'] = colors['--color-primary-light-400'] || normPrimary
    colors['--color-primary-subtle'] = colors['--color-primary-alpha-800'] || 'rgba(85, 196, 158, 0.18)'
    colors['--color-primary-border'] = colors['--color-primary-alpha-600'] || 'rgba(85, 196, 158, 0.4)'
    colors['--color-primary-text'] = colors['--color-primary-light-300'] || normPrimary
  } else {
    colors['--color-primary-hover'] = colors['--color-primary-dark-100'] || normPrimary
    colors['--color-primary-active'] = colors['--color-primary-dark-200'] || normPrimary
    colors['--color-primary-subtle'] = colors['--color-primary-alpha-900'] || 'rgba(22, 121, 97, 0.10)'
    colors['--color-primary-border'] = colors['--color-primary-alpha-600'] || 'rgba(22, 121, 97, 0.4)'
    colors['--color-primary-text'] = colors['--color-primary-dark-100'] || normPrimary
  }

  // Merge extInfo additions. Keys generated above (primary scale, font scale,
  // semantic tokens) are never overridden: the theme editor echoes previously
  // captured extInfo back verbatim, so letting it carry generated keys would
  // desync the editor's primaryColor from the effective --color-primary scale.
  if (extInfo) {
    for (const [key, value] of Object.entries(extInfo)) {
      if (value !== undefined && !Object.prototype.hasOwnProperty.call(colors, key)) {
        colors[key] = value
      }
    }
  }

  // 背景基调兜底：预设与主题编辑器都会写入这两个变量（style.css 的主要表面
  // 消费它们），缺失时给中性基调，保证任意主题下界面背景都有定义且透出亚克力磨砂。
  if (!colors['--color-app-background']) {
    colors['--color-app-background'] = isDark ? 'rgba(18, 21, 23, 0.55)' : 'rgba(245, 247, 249, 0.55)'
  }
  if (!colors['--color-main-background']) {
    colors['--color-main-background'] = isDark ? 'rgba(26, 30, 33, 0.82)' : 'rgba(255, 255, 255, 0.85)'
  }

  // 边框三档从主背景基调派生，背景换 tone 时边框随之变化；extInfo 可显式覆盖。
  // subtle = 发丝分割线，default = 常规组件描边，strong = 强调/可悬停描边。
  const mainBackground = colors['--color-main-background']
  if (!colors['--color-border-subtle']) {
    colors['--color-border-subtle'] = RGB_Linear_Shade(isDark ? 0.06 : -0.08, mainBackground)
  }
  if (!colors['--color-border']) {
    colors['--color-border'] = RGB_Linear_Shade(isDark ? 0.12 : -0.18, mainBackground)
  }
  if (!colors['--color-border-strong']) {
    colors['--color-border-strong'] = RGB_Linear_Shade(isDark ? 0.2 : -0.32, mainBackground)
  }

  return colors
}

function createFontColors(fontColor?: string, isDark = false, isDarkFont = false): Record<string, string> {
  const normFont = normalizeColor(fontColor ?? (isDark ? 'rgb(229, 229, 229)' : 'rgb(33, 33, 33)'))

  if (isDark) {
    return createFontDarkColors(normFont, isDarkFont)
  }

  const colors: Record<string, string> = {
    '--color-1000': normFont,
  }
  const step = (isDarkFont ? 0.02 : 0.05) * (isDark ? -1 : 1)
  for (let i = 1; i < 21; i += 1) {
    const key = `--color-${String(1000 - 50 * i).padStart(3, '0')}`
    colors[key] = RGB_Linear_Shade(step * i, normFont)
  }
  return colors
}

function createFontDarkColors(fontColor: string, isDarkFont = false): Record<string, string> {
  const colors: Record<string, string> = {
    '--color-1000': fontColor,
  }
  const step = isDarkFont ? -0.015 : -0.05
  let preColor = fontColor
  for (let i = 1; i < 21; i += 1) {
    preColor = RGB_Linear_Shade(step, preColor)
    const key = `--color-${String(1000 - 50 * i).padStart(3, '0')}`
    colors[key] = preColor
  }
  return colors
}
