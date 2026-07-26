import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'

const source = readFileSync(fileURLToPath(new URL('./components/SettingsDialog.vue', import.meta.url)), 'utf8')
const styleSource = readFileSync(fileURLToPath(new URL('./style.css', import.meta.url)), 'utf8')

function ruleFor(selector: string) {
  const escapedSelector = selector.replace(/[.*+?^\${}()|[\]\\]/g, '\\$&')
  return styleSource.match(new RegExp(escapedSelector + '\\s*\\{([^}]*)\\}'))?.[1]
}

describe('settings dialog motion affordances', () => {
  it('renders the MCP copy button with an animated icon slot and success state hook', () => {
    expect(source).toContain('mcp-copy-button')
    expect(source).toContain('class="mcp-copy-button__icon"')
    expect(source).toContain(':class="{ copied: mcpConfigCopied }"')
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
