import { describe, expect, it } from 'vitest'
import { createThemeColors } from './generator'

describe('theme generator', () => {
  it('generates full palette and semantic tokens for light theme', () => {
    const colors = createThemeColors('rgb(22, 121, 97)', 'rgb(33, 33, 33)', false)

    expect(colors['--color-primary']).toBe('rgb(22, 121, 97)')
    expect(colors['--color-primary-dark-100']).toBeDefined()
    expect(colors['--color-primary-dark-1000']).toBeDefined()
    expect(colors['--color-primary-light-100']).toBeDefined()
    expect(colors['--color-primary-light-1000']).toBeDefined()
    expect(colors['--color-primary-alpha-500']).toBeDefined()
    expect(colors['--color-theme']).toBe('rgb(22, 121, 97)')
    expect(colors['--color-1000']).toBe('rgb(33, 33, 33)')
    expect(colors['--color-primary-hover']).toBeDefined()
    expect(colors['--color-primary-active']).toBeDefined()
    expect(colors['--color-primary-subtle']).toBeDefined()
    expect(colors['--color-primary-text']).toBe('rgb(22, 121, 97)')
  })

  it('generates dark theme palette with inverted dark/light steps', () => {
    const colors = createThemeColors('rgb(85, 196, 158)', 'rgb(229, 229, 229)', true)

    expect(colors['--color-primary']).toBe('rgb(85, 196, 158)')
    expect(colors['--color-1000']).toBe('rgb(229, 229, 229)')
    expect(colors['--color-theme']).toBeDefined()
    expect(colors['--color-primary-hover']).toBeDefined()
    expect(colors['--color-primary-active']).toBeDefined()
    expect(colors['--color-primary-subtle']).toBeDefined()
  })

  it('merges extInfo overrides when provided', () => {
    const colors = createThemeColors('rgb(22, 121, 97)', undefined, false, false, {
      '--color-app-background': '#ffffff',
      '--color-btn-close': '#ff0000',
    })

    expect(colors['--color-app-background']).toBe('#ffffff')
    expect(colors['--color-btn-close']).toBe('#ff0000')
  })
})
