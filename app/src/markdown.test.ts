import { describe, expect, it } from 'vitest'
import { classifyMarkdownLink, renderMarkdown } from './markdown'
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

  it('does not replace [reference:N] inside code blocks', () => {
    const html = renderMarkdown('```python\nx = data[reference:1]\n```', message, new Map(), '')
    expect(html).not.toContain('class="reference-marker"')
    expect(html).toContain('data[reference:1]')
  })

  it('does not corrupt HTML entities when highlighting query matches', () => {
    const html = renderMarkdown('Tom &amp; Jerry &quot;Show&quot;', message, new Map(), 'amp')
    expect(html).not.toContain('&<mark')
    expect(html).toContain('&amp;')
  })

  it('renders math formulas with brackets and preserves bracket syntax in code blocks', () => {
    const html = renderMarkdown('\\[E = mc^2\\]\n\\(a^2 + b^2 = c^2\\)\n\n```python\nregex = r"\\[a-z\\]"\n```', message, new Map(), '')
    expect(html).toContain('class="katex"')
    expect(html).toContain('regex = r&quot;\\[a-z\\]&quot;')
    expect(html).not.toContain('$$a-z$$')
  })
})

describe('markdown link classification', () => {
  it('delegates network protocols to the system browser', () => {
    expect(classifyMarkdownLink('https://example.com/page')).toBe('open')
    expect(classifyMarkdownLink('HTTP://EXAMPLE.COM')).toBe('open')
    expect(classifyMarkdownLink('mailto:someone@example.com')).toBe('open')
  })

  it('keeps foreign and relative schemes inside the webview sandbox', () => {
    expect(classifyMarkdownLink('data:text/html,<h1>hi</h1>')).toBe('ignore')
    expect(classifyMarkdownLink('javascript:alert(1)')).toBe('ignore')
    expect(classifyMarkdownLink('file:///C:/Windows/System32')).toBe('ignore')
    expect(classifyMarkdownLink('#top')).toBe('ignore')
    expect(classifyMarkdownLink('')).toBe('ignore')
    expect(classifyMarkdownLink(null)).toBe('ignore')
    expect(classifyMarkdownLink(undefined)).toBe('ignore')
  })
})
