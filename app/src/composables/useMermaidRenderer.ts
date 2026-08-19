import { nextTick } from 'vue'

const mermaidOptions = {
  startOnLoad: false,
  securityLevel: 'strict' as const,
  fontFamily: 'Inter, Segoe UI, Microsoft YaHei, sans-serif',
}

export function normalizeMermaidSource(source: string) {
  return source.replace(/[“”]/g, '"')
}

export function useMermaidRenderer(effectiveTheme: () => 'light' | 'dark') {
  let instance: typeof import('mermaid')['default'] | null = null
  let renderVersion = 0

  async function loadMermaid() {
    if (!instance) {
      instance = (await import('mermaid')).default
      instance.initialize({
        ...mermaidOptions,
        theme: effectiveTheme() === 'dark' ? 'dark' : 'neutral',
      })
    }
    return instance
  }

  function reset() {
    instance = null
  }

  async function renderMermaidDiagrams() {
    const version = ++renderVersion
    await nextTick()
    const diagrams = [...document.querySelectorAll<HTMLElement>('.mermaid-diagram:not([data-rendered])')]
    if (!diagrams.length) return
    const t0 = performance.now()
    const mermaid = await loadMermaid()
    for (const [index, element] of diagrams.entries()) {
      if (version !== renderVersion) return
      const source = normalizeMermaidSource(decodeURIComponent(element.dataset.mermaidSource || ''))
      if (!source) continue
      try {
        const { svg, bindFunctions } = await mermaid.render(`mermaid-${version}-${index}`, source)
        element.innerHTML = svg
        element.dataset.rendered = 'true'
        bindFunctions?.(element)
      } catch (reason) {
        element.classList.add('mermaid-error')
        element.dataset.rendered = 'error'
        element.title = String(reason)
      }
    }
    console.debug(`[PERF:MERMAID] Rendered ${diagrams.length} diagrams in ${(performance.now() - t0).toFixed(2)}ms`)
  }

  async function renderExportMermaidDiagrams(root: HTMLElement) {
    const mermaid = (await import('mermaid')).default
    mermaid.initialize({ ...mermaidOptions, theme: 'neutral' })
    const diagrams = [...root.querySelectorAll<HTMLElement>('.mermaid-diagram:not([data-rendered])')]
    for (const [index, element] of diagrams.entries()) {
      const source = normalizeMermaidSource(decodeURIComponent(element.dataset.mermaidSource || ''))
      if (!source) continue
      try {
        const { svg } = await mermaid.render(`export-mermaid-${Date.now()}-${index}`, source)
        element.innerHTML = svg
        element.dataset.rendered = 'true'
      } catch (reason) {
        element.classList.add('mermaid-error')
        element.dataset.rendered = 'error'
        element.title = String(reason)
      }
    }
    reset()
  }

  return { renderMermaidDiagrams, renderExportMermaidDiagrams, reset }
}
