/** @vitest-environment happy-dom */

import { createApp, defineComponent, h, nextTick, ref } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Message, Reference } from './conversation'
import * as markdown from './markdown'
import MessageBlock from './MessageBlock.vue'

import { setLocale } from './i18n'
function oversizedContent(title = '完整标题') {
  return `# ${title}\n\n<strong>raw</strong>\n\n${'x'.repeat(100_000)}`
}

function messageFixture(overrides: Partial<Message> = {}): Message {
  return {
    id: 'message-1',
    role: 'assistant',
    content: 'short answer',
    metadata: {},
    seq: 0,
    ...overrides,
  }
}

function requiredElement<T extends Element>(root: ParentNode, selector: string) {
  const element = root.querySelector<T>(selector)
  if (!element) throw new Error(`Missing element ${selector}`)
  return element
}

function buttonWithText(root: ParentNode, text: string) {
  const button = [...root.querySelectorAll<HTMLButtonElement>('button')]
    .find((candidate) => candidate.textContent?.trim() === text)
  if (!button) throw new Error(`Missing button "${text}"`)
  return button
}

function mountMessage(initialMessage: Message, initialQuery = '', references = new Map<number, Reference>()) {
  document.body.innerHTML = '<div id="app"></div>'
  const message = ref(initialMessage)
  const query = ref(initialQuery)
  const contentRendered = vi.fn()
  const Root = defineComponent({
    setup: () => () => h(MessageBlock, {
      message: message.value,
      references,
      query: query.value,
      expanded: true,
      formattedDate: '2026-07-31',
      roleLabel: '助手',
      onContentRendered: contentRendered,
    }),
  })
  const app = createApp(Root)
  app.mount(requiredElement(document, '#app'))

  return {
    root: document.body,
    message,
    query,
    contentRendered,
    unmount() {
      app.unmount()
      document.body.innerHTML = ''
    },
  }
}

afterEach(() => {
  setLocale('zh-CN')
  vi.restoreAllMocks()
  document.body.innerHTML = ''
})

beforeEach(() => setLocale('zh-CN'))

describe('MessageBlock oversized content', () => {
  it('shows a surrogate-safe plain-text preview without invoking Markdown by default', () => {
    const renderMarkdown = vi.spyOn(markdown, 'renderMarkdown')
    const content = oversizedContent()
    const harness = mountMessage(messageFixture({ content }))
    try {
      const preview = requiredElement<HTMLElement>(harness.root, '.oversized-message-preview')

      expect(renderMarkdown).not.toHaveBeenCalled()
      expect(harness.root.querySelector('.markdown')).toBeNull()
      expect(preview.querySelector('strong')).toBeNull()
      expect(preview.textContent).toHaveLength(12_000)
      expect(harness.root.querySelector('.oversized-message-meta')?.textContent).toContain(content.length.toLocaleString('zh-CN'))
      expect(buttonWithText(harness.root, '渲染完整 Markdown')).toBeTruthy()
    } finally {
      harness.unmount()
    }
  })

  it('renders full Markdown on demand and can return to lightweight mode', async () => {
    const harness = mountMessage(messageFixture({ content: oversizedContent() }))
    try {
      buttonWithText(harness.root, '渲染完整 Markdown').click()
      await nextTick()

      expect(requiredElement(harness.root, '.markdown h1').textContent).toBe('完整标题')
      buttonWithText(harness.root, '恢复轻量模式').click()
      await nextTick()

      expect(harness.root.querySelector('.markdown')).toBeNull()
      expect(harness.root.querySelector('.oversized-message-preview')).not.toBeNull()
    } finally {
      harness.unmount()
    }
  })

  it('keeps full Markdown and search highlighting when the query is non-empty', () => {
    const harness = mountMessage(messageFixture({ content: oversizedContent('可搜索标题') }), '  可搜索  ')
    try {
      expect(requiredElement(harness.root, '.markdown h1')).not.toBeNull()
      expect(requiredElement(harness.root, 'mark.search-hit').textContent).toBe('可搜索')
      expect(harness.root.querySelector('.oversized-message-preview')).toBeNull()
    } finally {
      harness.unmount()
    }
  })

  it('resets an expanded oversized message when the message id changes', async () => {
    const harness = mountMessage(messageFixture({ content: oversizedContent('第一条') }))
    try {
      buttonWithText(harness.root, '渲染完整 Markdown').click()
      await nextTick()
      expect(requiredElement(harness.root, '.markdown h1').textContent).toBe('第一条')

      harness.message.value = messageFixture({ id: 'message-2', content: oversizedContent('第二条') })
      await nextTick()

      expect(harness.root.querySelector('.markdown')).toBeNull()
      expect(harness.root.querySelector('.oversized-message-preview')).not.toBeNull()
    } finally {
      harness.unmount()
    }
  })

  it('formats lightweight controls with the active English locale', async () => {
    setLocale('en-US')
    const content = oversizedContent()
    const harness = mountMessage(messageFixture({ content }))
    try {
      expect(harness.root.querySelector('.oversized-message-meta')?.textContent).toContain(
        `Original characters: ${new Intl.NumberFormat('en-US').format(content.length)}`,
      )
      expect(buttonWithText(harness.root, 'Render full Markdown')).toBeTruthy()
      buttonWithText(harness.root, 'Render full Markdown').click()
      await nextTick()
      expect(buttonWithText(harness.root, 'Restore lightweight mode')).toBeTruthy()
    } finally {
      harness.unmount()
    }
  })

  it('retains short-message references and expanded thinking rendering', () => {
    const reference: Reference = {
      cite_index: 1,
      url: 'https://example.test/source',
      title: 'Source',
      summary: 'Summary',
    }
    const harness = mountMessage(messageFixture({
      content: 'answer [reference:1]',
      metadata: { thinking: '**reasoning**' },
    }), '', new Map([[1, reference]]))
    try {
      expect(requiredElement(harness.root, '[data-search-field="content"] .reference-link')).not.toBeNull()
      expect(requiredElement(harness.root, '[data-search-field="thinking"] strong').textContent).toBe('reasoning')
      expect(harness.root.querySelector('.oversized-message-preview')).toBeNull()
    } finally {
      harness.unmount()
    }
  })
})
