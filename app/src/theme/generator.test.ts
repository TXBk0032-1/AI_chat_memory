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
    expect(colors['--color-primary-text']).toBe('rgb(20, 109, 87)')
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

  it('falls back to neutral background tones when extInfo omits them', () => {
    const light = createThemeColors('rgb(22, 121, 97)', undefined, false)
    expect(light['--color-app-background']).toBe('rgba(244, 245, 246, 0.65)')
    expect(light['--color-main-background']).toBe('rgba(255, 255, 255, 0.88)')

    const dark = createThemeColors('rgb(85, 196, 158)', undefined, true)
    expect(dark['--color-app-background']).toBe('rgba(17, 22, 26, 0.70)')
    expect(dark['--color-main-background']).toBe('rgba(32, 37, 40, 0.88)')
  })

  it('derives border tiers from the background tone and honors overrides', () => {
    const light = createThemeColors('rgb(22, 121, 97)', undefined, false)
    expect(light['--color-border-subtle']).toBe('rgba(235, 235, 235, 0.88)')
    expect(light['--color-border']).toBe('rgba(209, 209, 209, 0.88)')
    expect(light['--color-border-strong']).toBe('rgba(173, 173, 173, 0.88)')

    const dark = createThemeColors('rgb(85, 196, 158)', undefined, true)
    expect(dark['--color-border-subtle']).toBe('rgba(45, 50, 53, 0.88)')
    expect(dark['--color-border']).toBe('rgba(59, 63, 66, 0.88)')
    expect(dark['--color-border-strong']).toBe('rgba(77, 81, 83, 0.88)')

    const overridden = createThemeColors('rgb(22, 121, 97)', undefined, false, false, {
      '--color-border': '#123456',
    })
    expect(overridden['--color-border']).toBe('#123456')
    expect(overridden['--color-border-subtle']).toBe('rgba(235, 235, 235, 0.88)')
  })

  it('skips generator-derived keys when merging extInfo', () => {
    const colors = createThemeColors('rgb(22, 121, 97)', 'rgb(33, 33, 33)', false, false, {
      '--color-primary': '#ff0000',
      '--color-primary-hover': '#00ff00',
      '--color-primary-active': '#0000ff',
      '--color-primary-subtle': 'rgba(0, 0, 0, 0.1)',
      '--color-primary-border': 'rgba(0, 0, 0, 0.2)',
      '--color-primary-text': '#123456',
      '--color-primary-dark-100': '#010203',
      '--color-primary-light-1000': '#040506',
      '--color-primary-alpha-500': 'rgba(1, 2, 3, 0.5)',
      '--color-primary-dark-100-alpha-300': 'rgba(4, 5, 6, 0.3)',
      '--color-primary-light-1000-alpha-200': 'rgba(7, 8, 9, 0.2)',
      '--color-theme': '#000000',
      '--color-1000': '#111111',
      '--color-500': '#222222',
      // Non-generated keys keep merging as before.
      '--color-app-background': '#ffffff',
    })

    expect(colors['--color-primary']).toBe('rgb(22, 121, 97)')
    expect(colors['--color-primary-hover']).not.toBe('#00ff00')
    expect(colors['--color-primary-active']).not.toBe('#0000ff')
    expect(colors['--color-primary-subtle']).not.toBe('rgba(0, 0, 0, 0.1)')
    expect(colors['--color-primary-border']).not.toBe('rgba(0, 0, 0, 0.2)')
    expect(colors['--color-primary-text']).toBe('rgb(20, 109, 87)')
    expect(colors['--color-primary-dark-100']).not.toBe('#010203')
    expect(colors['--color-primary-light-1000']).not.toBe('#040506')
    expect(colors['--color-primary-alpha-500']).not.toBe('rgba(1, 2, 3, 0.5)')
    expect(colors['--color-primary-dark-100-alpha-300']).not.toBe('rgba(4, 5, 6, 0.3)')
    expect(colors['--color-primary-light-1000-alpha-200']).not.toBe('rgba(7, 8, 9, 0.2)')
    expect(colors['--color-theme']).toBe('rgb(22, 121, 97)')
    expect(colors['--color-1000']).toBe('rgb(33, 33, 33)')
    expect(colors['--color-500']).not.toBe('#222222')
    expect(colors['--color-app-background']).toBe('#ffffff')
  })

  it('still applies extInfo font-scale keys that the generator did not emit', () => {
    // The generator emits --color-000..--color-1000 in steps of 50; any other
    // --color-* key from extInfo must survive the merge untouched.
    const colors = createThemeColors('rgb(22, 121, 97)', 'rgb(33, 33, 33)', false, false, {
      '--color-nav-font': 'var(--color-primary)',
      '--color-075': '#abcdef',
    })
    expect(colors['--color-nav-font']).toBe('var(--color-primary)')
    expect(colors['--color-075']).toBe('#abcdef')
  })
})
