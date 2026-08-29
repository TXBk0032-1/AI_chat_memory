import type { Message } from './conversation'
import { translate as t } from './i18n'

export type ExportFormat = 'png' | 'jpeg' | 'pdf' | 'md' | 'json'

export type PdfExportOptions = {
  compact: boolean
  includeCoverPage: boolean
  includeThinking: boolean
}

export type ConversationTurn = {
  id: string
  seqs: number[]
}

export type ExportMessage = {
  role: string
  content: string
  created_at?: string
  thinking?: string
}

export type ConversationExport = {
  version: 1
  title: string
  time: string
  platform: string
  branch_id: string | null
  exported_at: string
  messages: ExportMessage[]
}

export type ConversationItem = Pick<Message, 'seq' | 'role'>

export const exportImagePixelRatio = 2
export const exportImageMaxDimension = 32767
export const exportImageMaxArea = 268_435_456

const invalidFilenameCharacters = /[<>:"/\\|?*\u0000-\u001f]/g
const reservedWindowsFilename = /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i

export function groupConversationTurns(items: ConversationItem[]): ConversationTurn[] {
  const ordered = [...items].sort((a, b) => a.seq - b.seq)
  const turns: ConversationTurn[] = []
  for (let index = 0; index < ordered.length; index += 1) {
    const item = ordered[index]
    const seqs = [item.seq]
    if (item.role === 'user' && ordered[index + 1]?.role === 'assistant') {
      seqs.push(ordered[index + 1].seq)
      index += 1
    }
    turns.push({ id: `turn-${seqs.join('-')}`, seqs })
  }
  return turns
}

export function selectedTurnSeqs(turns: ConversationTurn[], selectedIds: Set<string>): number[] {
  return turns.filter((turn) => selectedIds.has(turn.id)).flatMap((turn) => turn.seqs)
}

export function isImageExportTooLarge(width: number, height: number): boolean {
  const outputWidth = width * exportImagePixelRatio
  const outputHeight = height * exportImagePixelRatio
  return outputWidth > exportImageMaxDimension
    || outputHeight > exportImageMaxDimension
    || outputWidth * outputHeight > exportImageMaxArea
}

// Numeric timestamps above this magnitude are already in milliseconds; anything
// smaller is treated as seconds and scaled. Matches the heuristic used by
// formatDate in i18n/locale.ts so both stay consistent for the same values.
const millisecondsTimestampThreshold = 1e11

export function exportDate(value: string | undefined, fallback = new Date()): string {
  if (!value) return localDate(fallback)
  const numeric = Number(value)
  const date = Number.isFinite(numeric)
    ? new Date(Math.abs(numeric) > millisecondsTimestampThreshold ? numeric : numeric * 1000)
    : new Date(value)
  return Number.isNaN(date.valueOf()) ? localDate(fallback) : localDate(date)
}

function localDate(date: Date): string {
  const year = date.getFullYear()
  const month = String(date.getMonth() + 1).padStart(2, '0')
  const day = String(date.getDate()).padStart(2, '0')
  return `${year}-${month}-${day}`
}

export function sanitizeExportFilename(title: string, date: string, format: ExportFormat): string {
  const extension = format === 'jpeg' ? 'jpeg' : format
  let stem = title.replace(invalidFilenameCharacters, ' ').replace(/\s+/g, ' ').trim().replace(/[. ]+$/g, '')
  if (!stem) stem = t('app.untitledConversation')
  if (reservedWindowsFilename.test(stem)) stem = `_${stem}`
  const truncatedStem = Array.from(stem).slice(0, 100).join('').trim().replace(/[. ]+$/g, '')
  return `${truncatedStem || t('app.untitledConversation')}-${date}.${extension}`
}

export function toExportMessages(messages: Message[], includeThinking: boolean): ExportMessage[] {
  return [...messages]
    .sort((a, b) => a.seq - b.seq)
    .map((message) => {
      const thinking = includeThinking && typeof message.metadata?.thinking === 'string'
        ? message.metadata.thinking.trim()
        : ''
      return {
        role: message.role,
        content: message.content,
        ...(message.created_at ? { created_at: message.created_at } : {}),
        ...(thinking ? { thinking } : {}),
      }
    })
}

export function serializeMarkdown(model: ConversationExport): string {
  const blocks = model.messages.map((message) => {
    const role = message.role.toUpperCase()
    const parts: string[] = []
    if (message.thinking) parts.push(`**${t('export.thinkingLabel', { role })}**${t('export.separator')}\n${message.thinking}`)
    parts.push(`**${role}**${t('export.separator')}\n${message.content}`)
    return parts.join('\n\n')
  })
  return `> ${t('export.timeLabel')}: ${model.time}\n> ${t('export.platformLabel')}: ${model.platform}\n---\n${blocks.join('\n\n')}\n---\n`
}

export function serializeJson(model: ConversationExport): string {
  return `${JSON.stringify(model, null, 2)}\n`
}
