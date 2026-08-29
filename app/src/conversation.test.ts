import { beforeEach, describe, expect, it, vi } from 'vitest'
import { evictOldestReadingPositions, expandSearchHits, loadReadingPosition, mergeMessageBatch, readingPositionKey, resetReadingPositionsCache, saveReadingPosition, type Message, type ReadingPosition } from './conversation'

function message(seq: number): Message {
  return { id: `message-${seq}`, role: 'assistant', content: `${seq}`, metadata: {}, seq }
}

describe('conversation loading helpers', () => {
  beforeEach(() => {
    resetReadingPositionsCache()
    const values = new Map<string, string>()
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
    })
  })

  it('merges message batches by sequence without moving existing slots', () => {
    const slots = mergeMessageBatch(Array.from({ length: 6 }), [message(2), message(3)])
    const merged = mergeMessageBatch(slots, [message(0), message(3), message(5)])
    expect(merged.map((item) => item?.seq)).toEqual([0, undefined, 2, 3, undefined, 5])
  })

  it('expands server hit counts into stable navigation occurrences', () => {
    expect(expandSearchHits([{ message_id: 'one', seq: 4, field: 'thinking', count: 3 }]))
      .toEqual([
        { message_id: 'one', seq: 4, field: 'thinking', count: 3, occurrence: 0 },
        { message_id: 'one', seq: 4, field: 'thinking', count: 3, occurrence: 1 },
        { message_id: 'one', seq: 4, field: 'thinking', count: 3, occurrence: 2 },
      ])
  })

  it('persists and restores a reading sequence and offset', () => {
    saveReadingPosition('session', { seq: 480, offset: 27, updatedAt: 10 })
    expect(loadReadingPosition('session')).toEqual({ seq: 480, offset: 27, updatedAt: 10 })
  })

  it('keeps only the 500 most recently updated positions', () => {
    for (let index = 0; index < 505; index += 1) {
      saveReadingPosition(`session-${index}`, { seq: index, offset: 0, updatedAt: index })
    }
    expect(loadReadingPosition('session-0')).toBeNull()
    expect(loadReadingPosition('session-504')?.seq).toBe(504)
  })

  it('handles corrupted localStorage JSON without crashing and resets cache', () => {
    localStorage.setItem(readingPositionKey, 'invalid-json{{{')
    expect(loadReadingPosition('session-corrupt')).toBeNull()
    saveReadingPosition('session-new', { seq: 10, offset: 0, updatedAt: 1 })
    expect(loadReadingPosition('session-new')?.seq).toBe(10)
  })

  it('evicts the oldest half of the entries and reports exhaustion (FE-14)', () => {
    const position = (updatedAt: number): ReadingPosition => ({ seq: updatedAt, offset: 0, updatedAt })
    const entries = {
      'session-a': position(1),
      'session-b': position(2),
      'session-c': position(3),
      'session-d': position(4),
    }
    expect(evictOldestReadingPositions(entries)).toEqual({
      'session-c': position(3),
      'session-d': position(4),
    })
    expect(evictOldestReadingPositions({ 'session-a': position(1) })).toBeNull()
  })

  it('retries a quota-failed write after evicting the oldest entries (FE-14)', () => {
    const persisted = new Map<string, string>()
    const quotaBytes = 240
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => persisted.get(key) ?? null,
      setItem: (key: string, value: string) => {
        // Simulate a storage quota that only fits a handful of entries.
        if (value.length > quotaBytes) throw new DOMException('quota exceeded', 'QuotaExceededError')
        persisted.set(key, value)
      },
    })
    try {
      for (let index = 0; index < 20; index += 1) {
        saveReadingPosition(`session-${index}`, { seq: index, offset: 0, updatedAt: index })
      }
      const stored = JSON.parse(persisted.get(readingPositionKey) || '{}') as Record<string, ReadingPosition>
      // The retry evicted enough of the oldest entries to fit the quota.
      expect(Object.keys(stored).length).toBeGreaterThan(0)
      expect(Object.keys(stored).length).toBeLessThan(20)
      expect(stored['session-19']).toEqual({ seq: 19, offset: 0, updatedAt: 19 })
      expect(stored['session-0']).toBeUndefined()
      expect(loadReadingPosition('session-19')?.seq).toBe(19)
    } finally {
      vi.unstubAllGlobals()
    }
  })

  it('keeps the in-memory cache instead of wiping it when even the smallest write fails (FE-14)', () => {
    let writeAttempts = 0
    vi.stubGlobal('localStorage', {
      getItem: () => null,
      setItem: () => {
        writeAttempts += 1
        throw new DOMException('quota exceeded', 'QuotaExceededError')
      },
    })
    try {
      saveReadingPosition('session-a', { seq: 1, offset: 0, updatedAt: 1 })
      saveReadingPosition('session-b', { seq: 2, offset: 0, updatedAt: 2 })
      expect(writeAttempts).toBeGreaterThan(0)
      // The whole cache survives the degraded write: only persistence is lost.
      expect(loadReadingPosition('session-a')?.seq).toBe(1)
      expect(loadReadingPosition('session-b')?.seq).toBe(2)
    } finally {
      vi.unstubAllGlobals()
    }
  })
})
