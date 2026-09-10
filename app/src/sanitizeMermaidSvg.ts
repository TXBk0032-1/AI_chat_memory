import DOMPurify from 'dompurify'

/**
 * Single shared sanitization boundary for Mermaid-rendered SVG.
 *
 * Both the in-app renderer and the export renderer must pass Mermaid output
 * through this function before assigning it to `innerHTML`. The SVG profile
 * keeps the structure, styling and reference attributes Mermaid needs
 * (`id`, `class`, `style`, `viewBox`, `transform`, `d`, `marker-*`, `data-*`,
 * safe `href`).
 *
 * `foreignObject` is allowed (with `HTML_INTEGRATION_POINTS`, mirroring the
 * strict DOMPurify configuration in mermaid.core.mjs, which the input has
 * already passed through at securityLevel 'strict'): mermaid renders node
 * labels for flowcharts and state diagrams inside foreignObject, so removing
 * it would strip the label text. HTML elements inside the foreignObject
 * (div/span/p wrappers) are outside the svg profile's tag allowlist and are
 * stripped, but their text is hoisted up and kept (KEEP_CONTENT), so labels
 * render as plain text inside the foreignObject. `role`, `dominant-baseline`
 * and the `title` attribute (kept for mermaid's tooltip mechanism, which
 * reads `title="..."` off node groups) are outside the svg profile's
 * attribute allowlist and must be re-added explicitly. Executable content is
 * still forbidden: `script` tags, event-handler attributes (`onload`,
 * `onclick`, `onerror`, `onmouseover`, and any other `on*` attribute not in
 * the allowlist) and `javascript:` URIs are removed; DOMPurify's default
 * URL protocol filtering stays active (no `ALLOW_UNKNOWN_PROTOCOLS`).
 */
export function sanitizeMermaidSvg(svg: string): string {
  return DOMPurify.sanitize(svg, {
    USE_PROFILES: { svg: true, svgFilters: true },
    ADD_TAGS: ['foreignObject'],
    HTML_INTEGRATION_POINTS: { foreignobject: true },
    ADD_ATTR: ['role', 'dominant-baseline', 'title'],
    FORBID_TAGS: ['script'],
    FORBID_ATTR: ['onload', 'onclick', 'onerror', 'onmouseover'],
  })
}
