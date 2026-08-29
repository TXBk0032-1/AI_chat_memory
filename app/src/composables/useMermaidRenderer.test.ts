import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { normalizeMermaidSource, useMermaidRenderer } from './useMermaidRenderer'

const mermaid = vi.hoisted(() => ({
  initialize: vi.fn(),
  render: vi.fn(),
}))

vi.mock('mermaid', () => ({ default: mermaid }))

function diagram(source = 'graph TD\nA-->B') {
  return {
    dataset: { mermaidSource: encodeURIComponent(source) },
    innerHTML: '',
    title: '',
    classList: { add: vi.fn() },
  } as unknown as HTMLElement
}

describe('Mermaid rendering', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('normalizes curly quotes without changing straight quotes', () => {
    expect(normalizeMermaidSource('A[\u201cleft\u201d] B["right"]')).toBe('A["left"] B["right"]')
  })

  it('renders app diagrams with the effective theme and binds interactions', async () => {
    const element = diagram()
    const bindFunctions = vi.fn()
    mermaid.render.mockResolvedValueOnce({ svg: '<svg>app</svg>', bindFunctions })
    vi.stubGlobal('document', { querySelectorAll: vi.fn(() => [element]) })

    const renderer = useMermaidRenderer(() => 'dark')
    await renderer.renderMermaidDiagrams()

    expect(mermaid.initialize).toHaveBeenCalledWith(expect.objectContaining({ theme: 'dark', securityLevel: 'strict' }))
    expect(mermaid.render).toHaveBeenCalledWith('mermaid-1-0', 'graph TD\nA-->B')
    expect(element.innerHTML).toBe('<svg>app</svg>')
    expect(element.dataset.rendered).toBe('true')
    expect(bindFunctions).toHaveBeenCalledWith(element)
  })

  it('uses the neutral export theme and resets the app instance afterward', async () => {
    const exportElement = diagram()
    const appElement = diagram('graph TD\nB-->C')
    mermaid.render
      .mockResolvedValueOnce({ svg: '<svg>export</svg>' })
      .mockResolvedValueOnce({ svg: '<svg>app</svg>' })
    const root = { querySelectorAll: vi.fn(() => [exportElement]) } as unknown as HTMLElement

    const renderer = useMermaidRenderer(() => 'dark')
    await renderer.renderExportMermaidDiagrams(root)
    vi.stubGlobal('document', { querySelectorAll: vi.fn(() => [appElement]) })
    await renderer.renderMermaidDiagrams()

    expect(mermaid.initialize).toHaveBeenNthCalledWith(1, expect.objectContaining({ theme: 'neutral' }))
    expect(mermaid.initialize).toHaveBeenNthCalledWith(2, expect.objectContaining({ theme: 'dark' }))
    expect(exportElement.innerHTML).toBe('<svg>export</svg>')
    expect(appElement.innerHTML).toBe('<svg>app</svg>')
  })

  it('parks in-app renders until an in-flight export restores the app instance', async () => {
    let releaseExport!: (value: { svg: string }) => void
    const exportGate = new Promise<{ svg: string }>((resolve) => { releaseExport = resolve })
    const exportElement = diagram()
    const appElement = diagram('graph TD\nB-->C')
    mermaid.render
      .mockImplementationOnce(() => exportGate)
      .mockResolvedValueOnce({ svg: '<svg>app</svg>' })
    const root = { querySelectorAll: vi.fn(() => [exportElement]) } as unknown as HTMLElement

    const renderer = useMermaidRenderer(() => 'dark')
    const exportPromise = renderer.renderExportMermaidDiagrams(root)
    vi.stubGlobal('document', { querySelectorAll: vi.fn(() => [appElement]) })
    let appRendered = false
    const appPromise = renderer.renderMermaidDiagrams().then(() => { appRendered = true })

    // Drain pending microtasks: the app render must stay parked while the
    // export render still holds the shared instance.
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(appRendered).toBe(false)
    expect(appElement.innerHTML).toBe('')

    releaseExport({ svg: '<svg>export</svg>' })
    await Promise.all([exportPromise, appPromise])

    expect(appRendered).toBe(true)
    expect(exportElement.innerHTML).toBe('<svg>export</svg>')
    expect(appElement.innerHTML).toBe('<svg>app</svg>')
    // The app render re-initialized the shared instance with the app theme,
    // not the neutral export theme.
    expect(mermaid.initialize).toHaveBeenLastCalledWith(expect.objectContaining({ theme: 'dark' }))
  })

  it('restores the app instance when the export render fails', async () => {
    mermaid.initialize.mockImplementationOnce(() => {
      throw new Error('export init failed')
    })
    const appElement = diagram()
    mermaid.render.mockResolvedValueOnce({ svg: '<svg>app</svg>' })
    vi.stubGlobal('document', { querySelectorAll: vi.fn(() => [appElement]) })

    const renderer = useMermaidRenderer(() => 'dark')
    await expect(renderer.renderExportMermaidDiagrams({ querySelectorAll: vi.fn(() => []) } as unknown as HTMLElement)).rejects.toThrow('export init failed')
    await renderer.renderMermaidDiagrams()

    expect(appElement.innerHTML).toBe('<svg>app</svg>')
    expect(mermaid.initialize).toHaveBeenLastCalledWith(expect.objectContaining({ theme: 'dark' }))
  })

  it('renders only the diagrams inside the provided root', async () => {
    const scopedElement = diagram()
    const root = { querySelectorAll: vi.fn(() => [scopedElement]) } as unknown as ParentNode
    vi.stubGlobal('document', {
      querySelectorAll: vi.fn(() => {
        throw new Error('unexpected full document scan')
      }),
    })
    mermaid.render.mockResolvedValueOnce({ svg: '<svg>scoped</svg>' })

    const renderer = useMermaidRenderer(() => 'dark')
    await renderer.renderMermaidDiagrams(root)

    expect(root.querySelectorAll).toHaveBeenCalledWith('.mermaid-diagram:not([data-rendered])')
    expect(document.querySelectorAll).not.toHaveBeenCalled()
    expect(scopedElement.innerHTML).toBe('<svg>scoped</svg>')
    expect(scopedElement.dataset.rendered).toBe('true')
  })
})
