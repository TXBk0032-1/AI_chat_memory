import { describe, expect, it } from 'vitest'
import appSource from './App.vue?raw'

describe('App setup initialization order', () => {
  it('creates branch state before computed values and the virtualizer consume it', () => {
    const branchState = appSource.indexOf('const branches = useBranchNavigation')
    const displayedMessages = appSource.indexOf('const displayedMessageSeqs = computed')
    const virtualizer = appSource.indexOf('const messageVirtualizer = useVirtualizer')

    expect(branchState).toBeGreaterThan(-1)
    expect(branchState).toBeLessThan(displayedMessages)
    expect(displayedMessages).toBeLessThan(virtualizer)
  })

  it('delegates Mermaid rendering to its composable', () => {
    expect(appSource).toContain("import { useMermaidRenderer } from './composables/useMermaidRenderer'")
    expect(appSource).toContain('useMermaidRenderer(effectiveTheme)')
    expect(appSource).not.toContain("let mermaidInstance: typeof import('mermaid')['default'] | null = null")
  })
})
