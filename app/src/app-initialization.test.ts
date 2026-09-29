import { describe, expect, it } from 'vitest'
import appSource from './App.vue?raw'

describe('App setup initialization order', () => {
  it('delegates Mermaid rendering to its composable', () => {
    expect(appSource).toContain("import { useMermaidRenderer } from './composables/useMermaidRenderer'")
    expect(appSource).toContain('useMermaidRenderer(effectiveTheme)')
    expect(appSource).not.toContain("let mermaidInstance: typeof import('mermaid')['default'] | null = null")
  })

})
