import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { setLocale } from './i18n'
import {
  exportDate,
  groupConversationTurns,
  isImageExportTooLarge,
  sanitizeExportFilename,
  selectedTurnSeqs,
  serializeJson,
  serializeMarkdown,
  toExportMessages,
  type ConversationExport,
} from './conversation-export'

beforeEach(() => setLocale('zh-CN'))
afterEach(() => setLocale('zh-CN'))

describe('conversation export', () => {
  it('groups an adjacent user and assistant while preserving unmatched messages', () => {
    expect(groupConversationTurns([
      { seq: 4, role: 'assistant' },
      { seq: 1, role: 'user' },
      { seq: 2, role: 'assistant' },
      { seq: 3, role: 'user' },
      { seq: 5, role: 'system' },
    ])).toEqual([
      { id: 'turn-1-2', seqs: [1, 2] },
      { id: 'turn-3-4', seqs: [3, 4] },
      { id: 'turn-5', seqs: [5] },
    ])
  })

  it('keeps consecutive users and a trailing user independently selectable', () => {
    expect(groupConversationTurns([
      { seq: 0, role: 'user' },
      { seq: 1, role: 'user' },
      { seq: 2, role: 'assistant' },
      { seq: 3, role: 'user' },
    ]).map((turn) => turn.seqs)).toEqual([[0], [1, 2], [3]])
  })

  it('exports partial turn selections in conversation order', () => {
    const turns = [
      { id: 'first', seqs: [0, 1] },
      { id: 'second', seqs: [2, 3] },
      { id: 'third', seqs: [4, 5] },
    ]
    expect(selectedTurnSeqs(turns, new Set(['third', 'first']))).toEqual([0, 1, 4, 5])
    expect(selectedTurnSeqs(turns, new Set())).toEqual([])
  })

  it('serializes the requested markdown shape with optional thinking', () => {
    const model: ConversationExport = {
      version: 1,
      title: 'Example',
      time: '2025-07-14',
      platform: 'DeepSeek',
      branch_id: 'leaf',
      exported_at: '2026-07-15T00:00:00.000Z',
      messages: [
        { role: 'user', content: 'Hello' },
        { role: 'assistant', content: 'Hi', thinking: 'Consider it' },
      ],
    }
    expect(serializeMarkdown(model)).toBe(
      '> 时间: 2025-07-14\n> 平台: DeepSeek\n---\n**USER**：\nHello\n\n**ASSISTANT 思考过程**：\nConsider it\n\n**ASSISTANT**：\nHi\n---\n',
    )
    expect(JSON.parse(serializeJson(model))).toEqual(model)

    setLocale('en-US')
    expect(serializeMarkdown(model)).toBe(
      '> Time: 2025-07-14\n> Platform: DeepSeek\n---\n**USER**:\nHello\n\n**ASSISTANT reasoning**:\nConsider it\n\n**ASSISTANT**:\nHi\n---\n',
    )
  })

  it('only adds non-empty thinking when requested', () => {
    const messages = [
      { id: 'a', role: 'assistant', content: '', metadata: { thinking: '  plan  ' }, seq: 2 },
      { id: 'u', role: 'user', content: 'question', metadata: {}, seq: 1 },
    ]
    expect(toExportMessages(messages, false)).toEqual([
      { role: 'user', content: 'question' },
      { role: 'assistant', content: '' },
    ])
    expect(toExportMessages(messages, true)[1]).toMatchObject({ thinking: 'plan' })
  })

  it('formats dates and creates safe filenames', () => {
    const fallback = new Date(2025, 6, 14)
    expect(exportDate(undefined, fallback)).toBe('2025-07-14')
    expect(exportDate('invalid', fallback)).toBe('2025-07-14')
    expect(sanitizeExportFilename(' A/B:*?  ', '2025-07-14', 'jpeg')).toBe('A B-2025-07-14.jpeg')
    expect(sanitizeExportFilename(' My Conversation ', '2025-07-14', 'pdf')).toBe('My Conversation-2025-07-14.pdf')
    expect(sanitizeExportFilename('CON', '2025-07-14', 'md')).toBe('_CON-2025-07-14.md')

    // Truncates at unicode boundary without lone surrogates
    const longTitleWithEmoji = 'a'.repeat(99) + '🧠extra'
    const result = sanitizeExportFilename(longTitleWithEmoji, '2025-07-14', 'md')
    expect(result).toBe(`${'a'.repeat(99)}🧠-2025-07-14.md`)
    expect(() => encodeURIComponent(result)).not.toThrow()
  })

  it('detects image exports that exceed the canvas limits', () => {
    expect(isImageExportTooLarge(960, 10_000)).toBe(false)
    expect(isImageExportTooLarge(960, 16_384)).toBe(true)
    expect(isImageExportTooLarge(20_000, 100)).toBe(true)
  })
})
