// jsdom, not happy-dom: DOMPurify 3.4.x reads element names through the
// cached Node.prototype.nodeName getter, which happy-dom's base-class getter
// stubs to '', stripping every element; jsdom's HTML parser also handles
// <style>/<script> inside <svg> per spec.
/** @vitest-environment jsdom */

import { describe, expect, it } from 'vitest'
import { sanitizeMermaidSvg } from './sanitizeMermaidSvg'

describe('sanitizeMermaidSvg', () => {
  it('removes executable SVG content', () => {
    const dirty = '<svg onload="alert(1)"><script>alert(1)</script><foreignObject><div>html</div></foreignObject><a href="javascript:alert(1)">x</a><g onclick="x()"><text>safe</text></g></svg>'
    const clean = sanitizeMermaidSvg(dirty)
    expect(clean).toContain('<svg')
    expect(clean).toContain('<text>safe</text>')
    expect(clean).not.toMatch(/script|foreignObject|onload|onclick|javascript:/i)
  })

  it('keeps normal mermaid structure and styling', () => {
    const clean = sanitizeMermaidSvg('<svg viewBox="0 0 10 10"><style>.node{fill:#fff}</style><g id="n"><path d="M0 0L1 1"/><text>Node</text></g></svg>')
    expect(clean).toMatch(/viewBox="0 0 10 10"/i)
    expect(clean).toContain('<style>')
    expect(clean).toContain('<path')
    expect(clean).toContain('Node')
  })
})
