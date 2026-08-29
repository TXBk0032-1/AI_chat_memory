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
// Set while an export render is in flight so in-app renders can wait for the
// shared instance to be restored before touching it.
let exportCompletion: Promise<void> | null = null

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

  async function renderMermaidDiagrams(root: ParentNode = document) {
    const version = ++renderVersion
    await nextTick()
    // An export render temporarily reconfigures the shared instance with the
    // neutral export theme; wait for it to finish and restore the app state
    // before rendering in-app diagrams.
    await exportCompletion?.catch(() => {})
    if (version !== renderVersion) return
    const diagrams = [...root.querySelectorAll<HTMLElement>('.mermaid-diagram:not([data-rendered])')]
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
    const completion = runExportRender(root, version)
    exportCompletion = completion
    await completion
  }

  async function runExportRender(root: HTMLElement, version: number) {
    try {
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
    } finally {
      if (version === exportRenderVersion) {
        // Drop the shared instance so the next app render re-initializes it
        // with the current app theme, on every exit path (success, error or
        // superseded export). Without this, in-app diagrams rendered while the
        // export held the neutral theme would stay neutral.
        reset()
      }
    }
  }

  return { renderMermaidDiagrams, renderExportMermaidDiagrams, reset }
}
