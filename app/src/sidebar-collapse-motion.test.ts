import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import sidebarSource from './components/AppSidebar.vue?raw'

const styleSource = readFileSync(fileURLToPath(new URL('./style.css', import.meta.url)), 'utf8')

describe('sidebar collapse motion structure', () => {
  it('keeps one conversation count element through both sidebar states', () => {
    expect(sidebarSource).toContain('class="nav-item-count"')
    expect(sidebarSource).not.toContain('nav-item-collapsed')
  })

  it('uses continuous spring-like transforms for the count and source items', () => {
    expect(styleSource).toMatch(/\.app-frame[^}]*grid-template-columns \.46s cubic-bezier\(\.18, 1\.12, \.3, 1\)/)
    expect(styleSource).toMatch(/\.nav-item-count[^}]*transform \.46s cubic-bezier\(\.18, 1\.12, \.3, 1\)/)
    expect(styleSource).toMatch(/\.sidebar-collapsed \.nav-item-count[^}]*transform/)
    expect(styleSource).toMatch(/\.sidebar-collapsed \.source-item \.source-glyph[^}]*transform/)
    expect(styleSource).toMatch(/\.source-item i[^}]*width \.46s[^}]*height \.46s[^}]*font-size \.32s/)
    expect(styleSource).toMatch(/\.sidebar-collapsed \.source-item \.source-glyph[^}]*width:\s*25px[^}]*height:\s*25px[^}]*font-size:\s*13px/)
    expect(styleSource).toMatch(/\.sidebar-collapsed \.source-item > span[^}]*max-width:\s*0/)
    expect(styleSource).not.toMatch(/\.sidebar-collapsed \.nav-item-count[^}]*visibility:\s*hidden/)
    expect(styleSource).not.toMatch(/\.sidebar-collapsed \.source-glyph[^}]*visibility:\s*hidden/)
  })

  it('reduces the new transitions for reduced-motion users', () => {
    expect(styleSource).toMatch(/prefers-reduced-motion[^}]+\.nav-item-count/)
    expect(styleSource).toMatch(/prefers-reduced-motion[^}]+\.source-item > span/)
    expect(styleSource).toMatch(/prefers-reduced-motion[^}]+transition-duration:\s*\.01ms;\s*transition-delay:\s*0s/)
  })
})
