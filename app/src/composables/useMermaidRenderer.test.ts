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
})
