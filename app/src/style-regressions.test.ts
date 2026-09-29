import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import sidebarSource from './components/AppSidebar.vue?raw'

const styleSource = readFileSync(fileURLToPath(new URL('./style.css', import.meta.url)), 'utf8')
const settingsDialogSource = readFileSync(fileURLToPath(new URL('./components/SettingsDialog.vue', import.meta.url)), 'utf8')

function ruleFor(selector: string) {
  const escapedSelector = selector.replace(/[.*+?^\${}()|[\]\\]/g, '\\$&')
  return styleSource.match(new RegExp(escapedSelector + '\\s*\\{([^}]*)\\}'))?.[1]
}

describe('application motion enhancements and micro-interactions', () => {
  it('does not reorder session items with sliding motion and keeps session-state transition', () => {
    expect(styleSource).not.toContain('.session-item-move')
    expect(styleSource).toContain('.session-state-enter-active')
  })

  it('defines detail menu, alert bar, export toolbar and search navigation transitions', () => {
    expect(styleSource).toContain('.detail-menu-enter-active')
    expect(styleSource).toContain('.alert-bar-enter-active')
    expect(styleSource).toContain('.export-toolbar-enter-active')
    expect(styleSource).toContain('.search-nav-enter-active')
    expect(styleSource).not.toContain('.detail-pane-view-enter-active')
  })

  it('defines smooth ease-in and ease-out transitions for context menu', () => {
    expect(styleSource).toContain('.context-menu-enter-active')
    expect(styleSource).toContain('.context-menu-leave-active')
    expect(styleSource).toMatch(/\.context-menu\s*\{[^}]*transform-origin:\s*top left/)
  })

  it('adapts PDF export options to dark mode', () => {
    expect(styleSource).toMatch(/html\[data-theme="dark"\]\s+\.export-pdf-options\s*\{[^}]*background:/)
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

describe('date filter panel transition', () => {
  it('does not fade the panel background while it expands or collapses', () => {
    expect(styleSource).not.toMatch(/\.filter-panel-enter-active\s*\{[^}]*opacity/)
    expect(styleSource).not.toMatch(/\.filter-panel-leave-active\s*\{[^}]*opacity/)
    expect(styleSource).not.toMatch(/\.filter-panel-enter-from,\s*\.filter-panel-leave-to\s*\{[^}]*opacity/)
    expect(styleSource).not.toMatch(/\.filter-panel-enter-to,\s*\.filter-panel-leave-from\s*\{[^}]*opacity/)
  })
})

describe('settings dialog motion affordances', () => {
  it('renders the MCP copy button with an animated icon slot and success state hook', () => {
    expect(settingsDialogSource).toContain('mcp-copy-button')
    expect(settingsDialogSource).toContain('class="mcp-copy-button__icon"')
    expect(settingsDialogSource).toContain(':class="{ copied: mcpConfigCopied }"')
  })

  it('animates MCP copy feedback without abrupt width jumps', () => {
    const copyButton = ruleFor('.mcp-copy-button')
    const copiedButton = ruleFor('.mcp-copy-button.copied')
    const iconStroke = ruleFor('.mcp-copy-button__check polyline')

    expect(copyButton).toBeDefined()
    expect(copyButton).toMatch(/\bwidth:\s*136px\s*;/)
    expect(copyButton).toMatch(/\btransition:\s*[^;]*width[^;]*max-width[^;]*;/)
    expect(copyButton).toMatch(/\boverflow:\s*hidden\s*;/)
    expect(copiedButton).toBeDefined()
    expect(copiedButton).toMatch(/\bwidth:\s*152px\s*;/)
    expect(copiedButton).toMatch(/\bmax-width:\s*[^;]+;/)
    expect(iconStroke).toBeDefined()
    expect(iconStroke).toMatch(/\bstroke-dasharray:\s*[^;]+;/)
    expect(iconStroke).toMatch(/\btransition:\s*[^;]*stroke-dashoffset[^;]*;/)
  })

  it('gives setting switches a larger track radius and thumb size', () => {
    const switchTrack = ruleFor('.switch span')

    expect(switchTrack).toBeDefined()
    expect(switchTrack).toMatch(/\bborder-radius:\s*18px\s*;/)
    expect(styleSource).toMatch(/\.switch span::after\s*\{[^}]*\bwidth:\s*22px\s*;[^}]*\bheight:\s*22px\s*;/)
  })
})

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
