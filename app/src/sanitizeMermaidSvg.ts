import DOMPurify from 'dompurify'

/**
 * Single shared sanitization boundary for Mermaid-rendered SVG.
 *
 * Both the in-app renderer and the export renderer must pass Mermaid output
 * through this function before assigning it to `innerHTML`. The SVG profile
 * keeps the structure, styling and reference attributes Mermaid needs
 * (`id`, `class`, `style`, `viewBox`, `transform`, `d`, `marker-*`, `data-*`,
 * safe `href`), while executable content is explicitly forbidden. DOMPurify's
 * default URL protocol filtering stays active (no `ALLOW_UNKNOWN_PROTOCOLS`).
 */
export function sanitizeMermaidSvg(svg: string): string {
  return DOMPurify.sanitize(svg, {
    USE_PROFILES: { svg: true, svgFilters: true },
    FORBID_TAGS: ['script', 'foreignObject'],
    FORBID_ATTR: ['onload', 'onclick', 'onerror', 'onmouseover'],
  })
}
