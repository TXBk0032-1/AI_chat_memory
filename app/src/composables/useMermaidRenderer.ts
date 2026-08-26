import { nextTick } from 'vue'

const mermaidOptions = {
  startOnLoad: false,
  securityLevel: 'strict' as const,
  fontFamily: 'Inter, Segoe UI, Microsoft YaHei, sans-serif',
}

export function normalizeMermaidSource(source: string) {
  return source.replace(/[“”]/g, '"')
}

export function safeDecodeURIComponent(value: string): string {
  try {
    return decodeURIComponent(value)
  } catch {
    return value
  }
}

let exportSequence = 0

export function useMermaidRenderer(effectiveTheme: () => 'light' | 'dark') {
  let instance: typeof import('mermaid')['default'] | null = null
  let renderVersion = 0
  let exportRenderVersion = 0

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
      try {
        const rawSource = safeDecodeURIComponent(element.dataset.mermaidSource || '')
        const source = normalizeMermaidSource(rawSource)
        if (!source) continue
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
    const version = ++exportRenderVersion
    const mermaid = (await import('mermaid')).default
    mermaid.initialize({ ...mermaidOptions, theme: 'neutral' })
    const diagrams = [...root.querySelectorAll<HTMLElement>('.mermaid-diagram:not([data-rendered])')]
    for (const [index, element] of diagrams.entries()) {
      if (version !== exportRenderVersion) return
      try {
        const rawSource = safeDecodeURIComponent(element.dataset.mermaidSource || '')
        const source = normalizeMermaidSource(rawSource)
        if (!source) continue
        const { svg } = await mermaid.render(`export-mermaid-${++exportSequence}-${index}`, source)
        if (version !== exportRenderVersion) return
        element.innerHTML = svg
        element.dataset.rendered = 'true'
      } catch (reason) {
        if (version !== exportRenderVersion) return
        element.classList.add('mermaid-error')
        element.dataset.rendered = 'error'
        element.title = String(reason)
      }
    }
    if (version === exportRenderVersion) {
      reset()
    }
  }

  return { renderMermaidDiagrams, renderExportMermaidDiagrams, reset }
}
