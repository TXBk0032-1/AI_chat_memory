import { beforeEach, describe, expect, it, vi } from 'vitest'
import { expandSearchHits, loadReadingPosition, mergeMessageBatch, readingPositionKey, resetReadingPositionsCache, saveReadingPosition, type Message } from './conversation'

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
})
