export type SessionSummary = {
  id: string
  platform: string
  platform_session_id: string
  title: string
  created_at?: string
  updated_at?: string
  imported_at?: string
}

export type Message = {
  id: string
  role: string
  content: string
  metadata: Record<string, unknown>
  created_at?: string
  seq: number
}

export type Reference = {
  cite_index: number
  url: string
  title: string
  summary: string
}

export type SessionOpen = SessionSummary & {
  message_count: number
  has_branches: boolean
  start_seq: number
  messages: Message[]
  references: Reference[]
}

export type SearchHit = {
  message_id: string
  seq: number
  field: 'content' | 'thinking' | 'semantic'
  count: number
  score?: number
  snippet?: string
  chunk_id?: number
}

export type SearchMatch = SearchHit & { occurrence: number }

export type BranchNode = {
  message_id: string
  seq: number
  role: string
  node_id: string
  parent_node_id: string
  children_node_ids: string[]
  preview: string
}

export type BranchOverview = {
  nodes: BranchNode[]
  default_leaf_node_id: string
}

export type ReadingPosition = { seq: number; offset: number; updatedAt: number }

export const readingPositionKey = 'ai-chat-memory-reading-positions-v1'
const maxReadingPositions = 500

export function mergeMessageBatch(slots: Array<Message | undefined>, messages: Message[]) {
  const next = slots.slice()
  for (const message of messages) {
    if (message.seq >= 0 && message.seq < next.length) next[message.seq] = message
  }
  return next
}

export function expandSearchHits(hits: SearchHit[]): SearchMatch[] {
  return hits.flatMap((hit) => Array.from({ length: hit.count }, (_, occurrence) => ({ ...hit, occurrence })))
}

let cachedPositions: Record<string, ReadingPosition> | null = null

// Evicts the oldest half of the entries (by updatedAt) so a failing quota write
// can be retried with a smaller payload. Returns null once only a single entry
// remains, signalling that the write should be abandoned.
export function evictOldestReadingPositions(positions: Record<string, ReadingPosition>): Record<string, ReadingPosition> | null {
  const ordered = Object.entries(positions).sort((a, b) => b[1].updatedAt - a[1].updatedAt)
  const keep = Math.max(1, Math.floor(ordered.length / 2))
  if (keep >= ordered.length) return null
  return Object.fromEntries(ordered.slice(0, keep))
}

function writeReadingPositions(positions: Record<string, ReadingPosition>): Record<string, ReadingPosition> | null {
  let candidates = positions
  for (;;) {
    try {
      localStorage.setItem(readingPositionKey, JSON.stringify(candidates))
      return candidates
    } catch {
      // Quota pressure: shed the oldest entries and retry. The in-memory cache
      // is never wiped wholesale on a failed write; when even the smallest
      // payload does not fit, only this write is abandoned and the newest
      // positions remain available in memory.
      const reduced = evictOldestReadingPositions(candidates)
      if (!reduced) return null
      candidates = reduced
    }
  }
}

export function loadReadingPosition(sessionId: string): ReadingPosition | null {
  try {
    if (!cachedPositions) {
      cachedPositions = JSON.parse(localStorage.getItem(readingPositionKey) || '{}') as Record<string, ReadingPosition>
    }
    const position = cachedPositions[sessionId]
    return position && Number.isInteger(position.seq) ? position : null
  } catch {
    cachedPositions = {}
    return null
  }
}

export function saveReadingPosition(sessionId: string, position: ReadingPosition) {
  try {
    if (!cachedPositions) {
      cachedPositions = JSON.parse(localStorage.getItem(readingPositionKey) || '{}') as Record<string, ReadingPosition>
    }
    cachedPositions[sessionId] = position
    const entries = Object.entries(cachedPositions).sort((a, b) => b[1].updatedAt - a[1].updatedAt).slice(0, maxReadingPositions)
    const trimmed = Object.fromEntries(entries)
    const persisted = writeReadingPositions(trimmed)
    if (persisted) cachedPositions = persisted
  } catch {
    // Reading restoration is best-effort and must never block conversation rendering.
  }
}

export function resetReadingPositionsCache() {
  cachedPositions = null
}
