import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'

const styleSource = readFileSync(fileURLToPath(new URL('./style.css', import.meta.url)), 'utf8')

describe('application motion enhancements and micro-interactions', () => {
  it('defines fast and subtle session item list transitions', () => {
    expect(styleSource).toContain('.session-item-enter-active')
    expect(styleSource).toContain('.session-item-move')
    expect(styleSource).toContain('.session-state-enter-active')
  })

  it('defines detail menu, alert bar, export toolbar and search navigation transitions', () => {
    expect(styleSource).toContain('.detail-menu-enter-active')
    expect(styleSource).toContain('.alert-bar-enter-active')
    expect(styleSource).toContain('.export-toolbar-enter-active')
    expect(styleSource).toContain('.search-nav-enter-active')
    expect(styleSource).toContain('.detail-pane-view-enter-active')
  })

  it('defines active scale feedback for interactive buttons', () => {
    expect(styleSource).toMatch(/\.primary-button:active/)
    expect(styleSource).toMatch(/\.icon-button:active/)
  })

  it('defines smooth focus transitions for inputs', () => {
    expect(styleSource).toMatch(/\.search-field\s*\{[^}]*transition:[^}]*border-color/)
    expect(styleSource).toMatch(/\.filter-panel input\s*\{[^}]*transition:[^}]*border-color/)
  })

  it('defines settings nav highlight sliding capsule and expand transitions', () => {
    expect(styleSource).toContain('.settings-nav-highlight')
    expect(styleSource).toContain('.setting-expand-enter-active')
    expect(styleSource).toContain('.pdf-options-enter-active')
  })

  it('defines code block copy button styling and pop animation', () => {
    expect(styleSource).toContain('.code-copy-button')
    expect(styleSource).toContain('@keyframes copy-check-pop')
  })
})
