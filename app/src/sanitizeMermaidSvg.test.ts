// jsdom, not happy-dom: DOMPurify 3.4.x reads element names through the
// cached Node.prototype.nodeName getter, which happy-dom's base-class getter
// stubs to '', stripping every element; jsdom's HTML parser also handles
// <style>/<script> inside <svg> per spec.
/** @vitest-environment jsdom */

import { describe, expect, it } from 'vitest'
import { sanitizeMermaidSvg } from './sanitizeMermaidSvg'

describe('sanitizeMermaidSvg', () => {
  it('removes executable SVG content', () => {
    const dirty = '<svg onload="alert(1)"><script>alert(1)</script><foreignObject><iframe src="https://evil.example"></iframe><img src=x onerror="alert(1)"><div>html label</div></foreignObject><a href="javascript:alert(1)">x</a><g onclick="x()"><text>safe</text></g></svg>'
    const clean = sanitizeMermaidSvg(dirty)
    expect(clean).toContain('<svg')
    expect(clean).toContain('<text>safe</text>')
    expect(clean).not.toContain('<script')
    expect(clean).not.toContain('<iframe')
    expect(clean).not.toContain('onload=')
    expect(clean).not.toContain('onclick=')
    expect(clean).not.toContain('onerror=')
    expect(clean).not.toContain('javascript:')
    // The foreignObject itself (and its safe text) must survive: mermaid
    // renders flowchart/stateDiagram node labels inside foreignObject.
    expect(clean).toContain('<foreignObject')
    expect(clean).toContain('html label')
  })

  it('keeps normal mermaid structure and styling', () => {
    const clean = sanitizeMermaidSvg('<svg viewBox="0 0 10 10"><style>.node{fill:#fff}</style><g id="n"><path d="M0 0L1 1"/><text>Node</text></g></svg>')
    expect(clean).toMatch(/viewBox="0 0 10 10"/i)
    expect(clean).toContain('<style>')
    expect(clean).toContain('<path')
    expect(clean).toContain('Node')
  })

  it('keeps foreignObject label text mermaid emits for flowchart nodes', () => {
    // Mermaid renders htmlLabels node text inside foreignObject/div/span/p.
    // The svg profile keeps the foreignObject and its text (the inner HTML
    // wrappers are hoisted away by DOMPurify, the text survives); the node
    // label must not disappear from the sanitized SVG.
    const clean = sanitizeMermaidSvg('<svg><g class="node"><foreignObject width="80" height="40"><div xmlns="http://www.w3.org/1999/xhtml" style="color:red"><p><span class="nodeLabel"><p>Hello</p></span></p></div></foreignObject></g></svg>')
    expect(clean).toContain('<foreignObject')
    expect(clean).toContain('Hello')
  })

  it('keeps the title attribute mermaid tooltips rely on', () => {
    // securityLevel 'strict' mermaid writes click tooltips as a title
    // attribute on the node group; bindFunctions/setupToolTips reads it
    // back. The svg profile's attribute allowlist does not include title.
    const clean = sanitizeMermaidSvg('<svg><g class="node" title="my tooltip"><rect width="10" height="10"/></g></svg>')
    expect(clean).toContain('title="my tooltip"')
  })
})

// Real-mermaid regression: htmlLabels render node text inside foreignObject,
// so the sanitizer must not strip the label text out of genuine mermaid SVG
// (the FORBID_TAGS: ['foreignObject'] config once did). jsdom lacks real
// layout, so the geometry APIs mermaid measures with are stubbed.
/** @vitest-environment jsdom */
describe('sanitizeMermaidSvg with real mermaid output', () => {
  function stubLayoutApis() {
    const bbox = () => ({ x: 0, y: 0, width: 100, height: 40 })
    const rect = () => ({ x: 0, y: 0, top: 0, left: 0, width: 100, height: 40, right: 100, bottom: 40 })
    for (const proto of [
      (globalThis as any).SVGElement?.prototype,
      (globalThis as any).SVGGraphicsElement?.prototype,
      (globalThis as any).Element?.prototype,
      (globalThis as any).HTMLElement?.prototype,
    ]) {
      if (proto && !('getBBox' in proto)) (proto as any).getBBox = bbox
      if (proto) (proto as any).getBoundingClientRect = rect
    }
  }

  it('keeps flowchart node labels after sanitizing real mermaid SVG', async () => {
    stubLayoutApis()
    const mermaid = (await import('mermaid')).default
    mermaid.initialize({ startOnLoad: false, securityLevel: 'strict' })
    const { svg } = await mermaid.render('sanitize-flow-1', 'graph TD\nA[Hello]-->B[World]')
    const clean = sanitizeMermaidSvg(svg)
    expect(clean).toContain('<svg')
    expect(clean).toContain('Hello')
    expect(clean).toContain('World')
    expect(clean).not.toContain('<script')
  }, 120000)

  it('keeps state diagram node labels after sanitizing real mermaid SVG', async () => {
    stubLayoutApis()
    const mermaid = (await import('mermaid')).default
    mermaid.initialize({ startOnLoad: false, securityLevel: 'strict' })
    const { svg } = await mermaid.render('sanitize-state-1', 'stateDiagram-v2\n[*] --> Active\nActive --> Inactive')
    const clean = sanitizeMermaidSvg(svg)
    expect(clean).toContain('<svg')
    expect(clean).toContain('Active')
    expect(clean).toContain('Inactive')
    expect(clean).not.toContain('<script')
  }, 120000)
})
