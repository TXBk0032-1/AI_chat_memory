import { describe, expect, it } from 'vitest'
import { renderMarkdown } from './markdown'
import type { Message, Reference } from './conversation'

const message: Message = {
  id: 'message',
  role: 'assistant',
  content: '',
  metadata: {},
  seq: 0,
}

describe('markdown rendering', () => {
  it('renders compact session references as interactive previews', () => {
    const reference: Reference = {
      cite_index: 35,
      url: 'https://example.com/page',
      title: 'Example',
      summary: 'Summary',
    }
    const html = renderMarkdown('answer [reference:35]', message, new Map([[35, reference]]), '')
    expect(html).toContain('class="reference-link reference-marker"')
    expect(html).toContain('https://example.com/page')
    expect(html).toContain('Summary')
  })

  it('preserves KaTeX and deferred Mermaid rendering markers', () => {
    const html = renderMarkdown('\\(x^2\\)\n\n```mermaid\ngraph TD\nA-->B\n```', message, new Map(), '')
    expect(html).toContain('class="katex"')
    expect(html).toContain('class="mermaid-diagram"')
    expect(html).toContain('data-mermaid-source=')
  })

  it('wraps code fences in a container with language label and copy button', () => {
    const html = renderMarkdown('```typescript\nconst x = 1\n```', message, new Map(), '')
    expect(html).toContain('class="code-block-wrapper"')
    expect(html).toContain('class="code-block-header"')
    expect(html).toContain('class="code-block-lang"')
    expect(html).toContain('typescript')
    expect(html).toContain('class="code-copy-button"')
  })
})
