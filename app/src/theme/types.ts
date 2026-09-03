export type ThemeMode = 'system' | 'light' | 'dark'

export interface ThemeColors {
  '--color-primary': string
  '--color-theme': string
  [key: string]: string
}

export interface ThemeExtInfo {
  '--color-app-background'?: string
  '--color-main-background'?: string
  '--color-border'?: string
  '--color-border-subtle'?: string
  '--color-border-strong'?: string
  '--color-nav-font'?: string
  '--color-badge-primary'?: string
  '--color-badge-secondary'?: string
  '--color-badge-tertiary'?: string
  '--color-btn-close'?: string
  '--color-btn-min'?: string
  '--color-btn-hide'?: string
  '--background-image'?: string
  '--background-image-position'?: string
  '--background-image-size'?: string
  [key: string]: string | undefined
}

export interface ThemeConfig {
  primary: string
  font?: string
  extInfo?: ThemeExtInfo
}

export interface ThemeDefinition {
  id: string
  name: string
  nameKey: string
  isDark: boolean
  isDarkFont?: boolean
  isCustom?: boolean
  config: ThemeConfig
}

export interface ResolvedTheme {
  id: string
  name: string
  nameKey: string
  isDark: boolean
  isDarkFont: boolean
  primary: string
  colors: Record<string, string>
}
