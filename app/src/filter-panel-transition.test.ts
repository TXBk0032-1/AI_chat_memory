import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'

const styleSource = readFileSync(fileURLToPath(new URL('./style.css', import.meta.url)), 'utf8')

describe('date filter panel transition', () => {
  it('does not fade the panel background while it expands or collapses', () => {
    expect(styleSource).not.toMatch(/\.filter-panel-enter-active\s*\{[^}]*opacity/)
    expect(styleSource).not.toMatch(/\.filter-panel-leave-active\s*\{[^}]*opacity/)
    expect(styleSource).not.toMatch(/\.filter-panel-enter-from,\s*\.filter-panel-leave-to\s*\{[^}]*opacity/)
    expect(styleSource).not.toMatch(/\.filter-panel-enter-to,\s*\.filter-panel-leave-from\s*\{[^}]*opacity/)
  })
})
