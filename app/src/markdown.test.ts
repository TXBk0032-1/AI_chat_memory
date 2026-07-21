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

  it('escapes HTML special characters in search highlights', () => {
    const html = renderMarkdown('test content with <tags>', message, new Map(), 'content')
    // 确保 <tags> 被转义不会被当作真实 HTML 标签
    expect(html).toContain('&lt;tags&gt;')
    expect(html).not.toContain('<tags>')
    // 确保搜索词被高亮
    expect(html).toContain('<mark class="search-hit">content</mark>')
  })

  it('validates reference URLs and rejects invalid protocols', () => {
    const maliciousReference: Reference = {
      cite_index: 1,
      url: 'javascript:alert("xss")',
      title: 'Malicious',
      summary: 'This is malicious',
    }
    const html = renderMarkdown('[reference:1]', message, new Map([[1, maliciousReference]]), '')
    // 应该显示缺失引用而不是生成危险的链接
    expect(html).toContain('reference-missing')
    expect(html).not.toContain('javascript:')
  })

  it('allows valid http and https URLs in references', () => {
    const validHttpRef: Reference = {
      cite_index: 1,
      url: 'http://example.com',
      title: 'HTTP Example',
      summary: 'Summary',
    }
    const validHttpsRef: Reference = {
      cite_index: 2,
      url: 'https://example.com',
      title: 'HTTPS Example',
      summary: 'Summary',
    }
    const html = renderMarkdown('[reference:1] [reference:2]', message, new Map([[1, validHttpRef], [2, validHttpsRef]]), '')
    expect(html).toContain('reference-link')
    expect(html).toContain('http://example.com')
    expect(html).toContain('https://example.com')
  })
})
