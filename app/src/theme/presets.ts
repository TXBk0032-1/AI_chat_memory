import type { ThemeDefinition } from './types'

export const lightThemes: ThemeDefinition[] = [
  {
    id: 'green',
    name: 'Emerald Green',
    nameKey: 'theme.green',
    isDark: false,
    isDarkFont: false,
    config: {
      primary: 'rgb(22, 121, 97)',
      font: 'rgb(33, 33, 33)',
      extInfo: {
        '--color-app-background': 'rgba(245, 247, 249, 0.65)',
        '--color-main-background': 'rgba(255, 255, 255, 0.92)',
        '--color-nav-font': 'var(--color-primary)',
        '--color-btn-hide': '#3bc2b2',
        '--color-btn-min': '#85c43b',
        '--color-btn-close': '#fab4a0',
        '--color-badge-primary': 'var(--color-primary)',
        '--color-badge-secondary': '#4baed5',
        '--color-badge-tertiary': '#e7aa36',
      },
    },
  },
  {
    id: 'lx_green',
    name: 'Verdant Green',
    nameKey: 'theme.lxGreen',
    isDark: false,
    isDarkFont: false,
    config: {
      primary: 'rgb(77, 175, 124)',
      font: 'rgb(33, 33, 33)',
      extInfo: {
        '--color-app-background': 'rgba(245, 247, 249, 0.65)',
        '--color-main-background': 'rgba(255, 255, 255, 0.92)',
        '--color-nav-font': 'var(--color-primary)',
        '--color-btn-hide': '#3bc2b2',
        '--color-btn-min': '#85c43b',
        '--color-btn-close': '#fab4a0',
        '--color-badge-primary': 'var(--color-primary)',
        '--color-badge-secondary': '#4baed5',
        '--color-badge-tertiary': '#e7aa36',
      },
    },
  },
  {
    id: 'blue',
    name: 'Sapphire Jade',
    nameKey: 'theme.blue',
    isDark: false,
    isDarkFont: false,
    config: {
      primary: 'rgb(52, 152, 219)',
      font: 'rgb(33, 33, 33)',
      extInfo: {
        '--color-app-background': 'rgba(245, 247, 249, 0.65)',
        '--color-main-background': 'rgba(255, 255, 255, 0.92)',
        '--color-nav-font': 'var(--color-primary)',
        '--color-badge-primary': 'var(--color-primary)',
        '--color-badge-secondary': '#5cbf9b',
        '--color-badge-tertiary': '#5cbf9b',
      },
    },
  },
  {
    id: 'blue_plus',
    name: 'Eggshell Blue',
    nameKey: 'theme.bluePlus',
    isDark: false,
    isDarkFont: false,
    config: {
      primary: 'rgb(77, 131, 175)',
      font: 'rgb(33, 33, 33)',
      extInfo: {
        '--color-app-background': 'rgba(245, 247, 249, 0.65)',
        '--color-main-background': 'rgba(255, 255, 255, 0.92)',
        '--color-nav-font': 'var(--color-primary)',
        '--color-badge-primary': 'var(--color-primary)',
        '--color-badge-secondary': 'rgba(66, 150, 171, 1)',
        '--color-badge-tertiary': 'rgba(54, 196, 231, 1)',
      },
    },
  },
  {
    id: 'orange',
    name: 'Amber Orange',
    nameKey: 'theme.orange',
    isDark: false,
    isDarkFont: false,
    config: {
      primary: 'rgb(245, 171, 53)',
      font: 'rgb(33, 33, 33)',
      extInfo: {
        '--color-app-background': 'rgba(245, 247, 249, 0.65)',
        '--color-main-background': 'rgba(255, 255, 255, 0.92)',
        '--color-nav-font': 'var(--color-primary)',
        '--color-badge-primary': 'var(--color-primary)',
        '--color-badge-secondary': '#9ed458',
        '--color-badge-tertiary': '#9ed458',
      },
    },
  },
  {
    id: 'red',
    name: 'Crimson Flame',
    nameKey: 'theme.red',
    isDark: false,
    isDarkFont: false,
    config: {
      primary: 'rgb(214, 69, 65)',
      font: 'rgb(33, 33, 33)',
      extInfo: {
        '--color-app-background': 'rgba(245, 247, 249, 0.65)',
        '--color-main-background': 'rgba(255, 255, 255, 0.92)',
        '--color-nav-font': 'var(--color-primary)',
        '--color-badge-primary': 'var(--color-primary)',
        '--color-badge-secondary': '#dfbb6b',
        '--color-badge-tertiary': '#dfbb6b',
      },
    },
  },
  {
    id: 'pink',
    name: 'Sakura Peach',
    nameKey: 'theme.pink',
    isDark: false,
    isDarkFont: false,
    config: {
      primary: 'rgb(241, 130, 141)',
      font: 'rgb(33, 33, 33)',
      extInfo: {
        '--color-app-background': 'rgba(245, 247, 249, 0.65)',
        '--color-main-background': 'rgba(255, 255, 255, 0.92)',
        '--color-nav-font': 'var(--color-primary)',
        '--color-badge-primary': 'var(--color-primary)',
        '--color-badge-secondary': '#f5b684',
        '--color-badge-tertiary': '#f5b684',
      },
    },
  },
  {
    id: 'purple',
    name: 'Amethyst Purple',
    nameKey: 'theme.purple',
    isDark: false,
    isDarkFont: false,
    config: {
      primary: 'rgb(155, 89, 182)',
      font: 'rgb(33, 33, 33)',
      extInfo: {
        '--color-app-background': 'rgba(245, 247, 249, 0.65)',
        '--color-main-background': 'rgba(255, 255, 255, 0.92)',
        '--color-nav-font': 'var(--color-primary)',
        '--color-badge-primary': 'var(--color-primary)',
        '--color-badge-secondary': '#e5a39f',
        '--color-badge-tertiary': '#e5a39f',
      },
    },
  },
  {
    id: 'ming',
    name: 'Cyan Obsidian',
    nameKey: 'theme.ming',
    isDark: false,
    isDarkFont: false,
    config: {
      primary: 'rgb(51, 110, 123)',
      font: 'rgb(33, 33, 33)',
      extInfo: {
        '--color-app-background': 'rgba(245, 247, 249, 0.65)',
        '--color-main-background': 'rgba(255, 255, 255, 0.92)',
        '--color-nav-font': 'var(--color-primary)',
        '--color-badge-primary': 'var(--color-primary)',
        '--color-badge-secondary': '#6376a2',
        '--color-badge-tertiary': '#6376a2',
      },
    },
  },
  {
    id: 'grey',
    name: 'Charcoal Mist',
    nameKey: 'theme.grey',
    isDark: false,
    isDarkFont: false,
    config: {
      primary: 'rgb(108, 122, 137)',
      font: 'rgb(33, 33, 33)',
      extInfo: {
        '--color-app-background': 'rgba(245, 247, 249, 0.65)',
        '--color-main-background': 'rgba(255, 255, 255, 0.92)',
        '--color-nav-font': 'var(--color-primary)',
        '--color-badge-primary': 'var(--color-primary)',
        '--color-badge-secondary': '#b19b9f',
        '--color-badge-tertiary': '#b19b9f',
      },
    },
  },
  {
    id: 'blue2',
    name: 'Indigo Wave',
    nameKey: 'theme.blue2',
    isDark: false,
    isDarkFont: false,
    config: {
      primary: 'rgb(79, 98, 208)',
      font: 'rgb(33, 33, 33)',
      extInfo: {
        '--color-app-background': 'rgba(245, 247, 249, 0.65)',
        '--color-main-background': 'rgba(255, 255, 255, 0.92)',
        '--color-nav-font': 'var(--color-primary)',
        '--color-badge-primary': 'var(--color-primary)',
        '--color-badge-secondary': '#b080db',
        '--color-badge-tertiary': '#b080db',
      },
    },
  },
]

export const darkThemes: ThemeDefinition[] = [
  {
    id: 'black',
    name: 'Obsidian Night',
    nameKey: 'theme.black',
    isDark: true,
    isDarkFont: false,
    config: {
      primary: 'rgb(85, 196, 158)',
      font: 'rgb(229, 229, 229)',
      extInfo: {
        '--color-app-background': 'rgba(18, 21, 23, 0.70)',
        '--color-main-background': 'rgba(26, 30, 33, 0.92)',
        '--color-nav-font': 'var(--color-primary)',
        '--color-badge-primary': 'var(--color-primary)',
        '--color-badge-secondary': '#55c49e',
        '--color-badge-tertiary': '#e0aa4a',
      },
    },
  },
  {
    id: 'dark_emerald',
    name: 'Dark Emerald',
    nameKey: 'theme.darkEmerald',
    isDark: true,
    isDarkFont: false,
    config: {
      primary: 'rgb(57, 173, 141)',
      font: 'rgb(229, 229, 229)',
      extInfo: {
        '--color-app-background': 'rgba(18, 21, 23, 0.70)',
        '--color-main-background': 'rgba(26, 30, 33, 0.92)',
        '--color-nav-font': 'var(--color-primary)',
      },
    },
  },
  {
    id: 'dark_blue',
    name: 'Midnight Blue',
    nameKey: 'theme.darkBlue',
    isDark: true,
    isDarkFont: false,
    config: {
      primary: 'rgb(96, 165, 250)',
      font: 'rgb(229, 229, 229)',
      extInfo: {
        '--color-app-background': 'rgba(18, 21, 23, 0.70)',
        '--color-main-background': 'rgba(26, 30, 33, 0.92)',
        '--color-nav-font': 'var(--color-primary)',
      },
    },
  },
  {
    id: 'dark_purple',
    name: 'Cyber Purple',
    nameKey: 'theme.darkPurple',
    isDark: true,
    isDarkFont: false,
    config: {
      primary: 'rgb(167, 139, 250)',
      font: 'rgb(229, 229, 229)',
      extInfo: {
        '--color-app-background': 'rgba(18, 21, 23, 0.70)',
        '--color-main-background': 'rgba(26, 30, 33, 0.92)',
        '--color-nav-font': 'var(--color-primary)',
      },
    },
  },
  {
    id: 'dark_orange',
    name: 'Warm Sunset',
    nameKey: 'theme.darkOrange',
    isDark: true,
    isDarkFont: false,
    config: {
      primary: 'rgb(251, 146, 60)',
      font: 'rgb(229, 229, 229)',
      extInfo: {
        '--color-app-background': 'rgba(18, 21, 23, 0.70)',
        '--color-main-background': 'rgba(26, 30, 33, 0.92)',
        '--color-nav-font': 'var(--color-primary)',
      },
    },
  },
  {
    id: 'dark_charcoal',
    name: 'Pitch Charcoal',
    nameKey: 'theme.darkCharcoal',
    isDark: true,
    isDarkFont: false,
    config: {
      primary: 'rgb(203, 213, 225)',
      font: 'rgb(229, 229, 229)',
      extInfo: {
        '--color-app-background': 'rgba(18, 21, 23, 0.70)',
        '--color-main-background': 'rgba(26, 30, 33, 0.92)',
        '--color-nav-font': 'var(--color-primary)',
      },
    },
  },
]

export const allThemes: ThemeDefinition[] = [...lightThemes, ...darkThemes]

export function findTheme(id: string, customThemes: ThemeDefinition[] = []): ThemeDefinition | undefined {
  return customThemes.find((t) => t.id === id) || allThemes.find((t) => t.id === id)
}

export function getDefaultTheme(isDark: boolean, customThemes: ThemeDefinition[] = []): ThemeDefinition {
  if (isDark) {
    const customDark = customThemes.find((t) => t.isDark)
    return customDark || darkThemes[0]
  }
  const customLight = customThemes.find((t) => !t.isDark)
  return customLight || lightThemes[0]
}

export function getCategorizedThemes(customThemes: ThemeDefinition[] = []): {
  customLight: ThemeDefinition[]
  customDark: ThemeDefinition[]
  presetLight: ThemeDefinition[]
  presetDark: ThemeDefinition[]
} {
  return {
    customLight: customThemes.filter((t) => !t.isDark),
    customDark: customThemes.filter((t) => t.isDark),
    presetLight: lightThemes,
    presetDark: darkThemes,
  }
}

export function createDefaultCustomTheme(isDark: boolean, name = ''): ThemeDefinition {
  const id = `custom_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`
  if (isDark) {
    return {
      id,
      name: name || 'Custom Dark',
      nameKey: '',
      isDark: true,
      isDarkFont: false,
      isCustom: true,
      config: {
        primary: 'rgb(85, 196, 158)',
        font: 'rgb(229, 229, 229)',
        extInfo: {
          '--color-app-background': 'rgba(18, 21, 23, 0.70)',
          '--color-main-background': 'rgba(26, 30, 33, 0.92)',
          '--color-nav-font': 'var(--color-primary)',
          '--color-badge-primary': 'var(--color-primary)',
          '--color-badge-secondary': '#3ba272',
          '--color-badge-tertiary': '#60c9a4',
        },
      },
    }
  }

  return {
    id,
    name: name || 'Custom Light',
    nameKey: '',
    isDark: false,
    isDarkFont: false,
    isCustom: true,
    config: {
      primary: 'rgb(22, 121, 97)',
      font: 'rgb(33, 33, 33)',
      extInfo: {
        '--color-app-background': 'rgba(245, 247, 249, 0.65)',
        '--color-main-background': 'rgba(255, 255, 255, 0.92)',
        '--color-nav-font': 'var(--color-primary)',
        '--color-btn-hide': '#3bc2b2',
        '--color-btn-min': '#85c43b',
        '--color-btn-close': '#fab4a0',
        '--color-badge-primary': 'var(--color-primary)',
        '--color-badge-secondary': '#4baed5',
        '--color-badge-tertiary': '#e7aa36',
      },
    },
  }
}

export function duplicateThemeAsCustom(source: ThemeDefinition, newName?: string): ThemeDefinition {
  const id = `custom_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`
  return {
    id,
    name: newName || `${source.name} (Copy)`,
    nameKey: '',
    isDark: source.isDark,
    isDarkFont: Boolean(source.isDarkFont),
    isCustom: true,
    config: {
      primary: source.config.primary,
      font: source.config.font,
      extInfo: source.config.extInfo ? { ...source.config.extInfo } : undefined,
    },
  }
}
