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
        '--color-app-background': 'rgba(247, 249, 250, 0.9)',
        '--color-main-background': 'rgba(255, 255, 255, 1)',
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
        '--color-app-background': 'rgba(247, 249, 250, 0.9)',
        '--color-main-background': 'rgba(255, 255, 255, 1)',
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
        '--color-app-background': 'rgba(244, 247, 251, 0.9)',
        '--color-main-background': 'rgba(255, 255, 255, 1)',
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
        '--color-app-background': 'rgba(243, 246, 249, 0.9)',
        '--color-main-background': 'rgba(255, 255, 255, 1)',
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
        '--color-app-background': 'rgba(253, 249, 242, 0.9)',
        '--color-main-background': 'rgba(255, 255, 255, 1)',
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
        '--color-app-background': 'rgba(253, 245, 245, 0.9)',
        '--color-main-background': 'rgba(255, 255, 255, 1)',
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
        '--color-app-background': 'rgba(254, 246, 247, 0.9)',
        '--color-main-background': 'rgba(255, 255, 255, 1)',
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
        '--color-app-background': 'rgba(250, 245, 253, 0.9)',
        '--color-main-background': 'rgba(255, 255, 255, 1)',
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
        '--color-app-background': 'rgba(243, 247, 248, 0.9)',
        '--color-main-background': 'rgba(255, 255, 255, 1)',
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
        '--color-app-background': 'rgba(245, 246, 248, 0.9)',
        '--color-main-background': 'rgba(255, 255, 255, 1)',
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
        '--color-app-background': 'rgba(244, 246, 254, 0.9)',
        '--color-main-background': 'rgba(255, 255, 255, 1)',
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
        '--color-app-background': 'rgba(23, 27, 30, 0.95)',
        '--color-main-background': 'rgba(32, 37, 40, 1)',
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
        '--color-app-background': 'rgba(20, 28, 26, 0.95)',
        '--color-main-background': 'rgba(27, 38, 35, 1)',
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
        '--color-app-background': 'rgba(15, 23, 42, 0.95)',
        '--color-main-background': 'rgba(23, 33, 56, 1)',
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
        '--color-app-background': 'rgba(24, 18, 43, 0.95)',
        '--color-main-background': 'rgba(34, 27, 58, 1)',
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
        '--color-app-background': 'rgba(28, 25, 23, 0.95)',
        '--color-main-background': 'rgba(38, 34, 32, 1)',
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
        '--color-app-background': 'rgba(18, 18, 18, 0.95)',
        '--color-main-background': 'rgba(26, 26, 26, 1)',
        '--color-nav-font': 'var(--color-primary)',
      },
    },
  },
]

export const allThemes: ThemeDefinition[] = [...lightThemes, ...darkThemes]

export function findTheme(id: string): ThemeDefinition | undefined {
  return allThemes.find((t) => t.id === id)
}

export function getDefaultTheme(isDark: boolean): ThemeDefinition {
  if (isDark) {
    return darkThemes[0]
  }
  return lightThemes[0]
}
