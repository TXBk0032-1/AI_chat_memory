import MarkdownIt from 'markdown-it'
import texmath from 'markdown-it-texmath'
import katex from 'katex'
import type { Message, Reference } from './conversation'

const markdown = new MarkdownIt({ html: false, linkify: true, breaks: true }).use(texmath, {
  engine: katex,
  delimiters: ['dollars', 'brackets'],
})

const defaultFence = markdown.renderer.rules.fence
markdown.renderer.rules.fence = (tokens, index, options, environment, renderer) => {
  const token = tokens[index]
  if (token.info.trim().toLowerCase() === 'mermaid') {
    return `<div class="mermaid-diagram" data-mermaid-source="${encodeURIComponent(token.content)}"><pre>${markdown.utils.escapeHtml(token.content)}</pre></div>`
  }
  return defaultFence ? defaultFence(tokens, index, options, environment, renderer) : renderer.renderToken(tokens, index, options)
}

function referenceValue(reference: unknown, keys: string[]) {
  if (!reference || typeof reference !== 'object') return ''
  const item = reference as Record<string, unknown>
  for (const key of keys) if (typeof item[key] === 'string') return item[key] as string
  return ''
}

function resolveReference(index: number, message: Message, references: Map<number, Reference>) {
  const messageReferences = Array.isArray(message.metadata?.references) ? message.metadata.references : []
  return references.get(index)
    ?? messageReferences.find((reference) => Number((reference as Record<string, unknown>)?.cite_index) === index)
    ?? messageReferences[index]
    ?? messageReferences[index - 1]
}

function escapeRegExp(value: string) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

function escapeHtmlForHighlight(text: string) {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;')
}

function highlightRenderedHtml(html: string, query: string) {
  const needle = query.trim()
  if (!needle) return html
  const pattern = new RegExp(escapeRegExp(needle), 'gi')
  return html.split(/(<[^>]+>)/g).map((part) => {
    if (part.startsWith('<')) return part
    return part.replace(pattern, (match) => {
      const escaped = escapeHtmlForHighlight(match)
      return `<mark class="search-hit">${escaped}</mark>`
    })
  }).join('')
}

function validateReferenceUrl(url: string): boolean {
  if (!url || typeof url !== 'string') return false
  try {
    const parsed = new URL(url)
    // 只允许 http 和 https 协议
    return ['http:', 'https:'].includes(parsed.protocol)
  } catch {
    return false
  }
}

export function renderMarkdown(value: string, message: Message, references: Map<number, Reference>, query: string) {
  const source = (value || '')
    .replace(/\\\[([\s\S]*?)\\\]/g, (_, formula) => `\n$$${formula}$$\n`)
    .replace(/\\\((.+?)\\\)/g, (_, formula) => `$${formula}$`)
  const rendered = markdown.render(source).replace(/\[reference:(\d+)\]/gi, (_match, rawIndex) => {
    const index = Number(rawIndex)
    const reference = resolveReference(index, message, references)
    const url = referenceValue(reference, ['url', 'link', 'href'])
    
    // 验证 URL 的有效性和安全性
    if (!validateReferenceUrl(url)) {
      return `<span class="reference-marker reference-missing" title="该引用来源未随历史记录保存">${index}</span>`
    }
    
    const title = referenceValue(reference, ['title', 'name']) || `引用 ${index}`
    const summary = referenceValue(reference, ['snippet', 'summary', 'description', 'content']).replace(/\s+/g, ' ').slice(0, 280)
    const safeTitle = markdown.utils.escapeHtml(title)
    const safeSummary = markdown.utils.escapeHtml(summary)
    const safeUrl = markdown.utils.escapeHtml(url)
    return `<a class="reference-link reference-marker" href="${safeUrl}" target="_blank" rel="noopener noreferrer">${index}<span class="reference-preview"><strong>${safeTitle}</strong><span>${safeSummary || '暂无摘要'}</span><small>${safeUrl}</small></span></a>`
  })
  return highlightRenderedHtml(rendered, query)
}

export function escapeTitle(value: string) {
  return markdown.utils.escapeHtml(value || '未命名对话')
}
