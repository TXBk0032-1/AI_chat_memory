import { ref } from 'vue'
import { describe, expect, it, vi } from 'vitest'
import type { DesktopApi } from '../desktop-api'
import { useConversationSearch } from './useConversationSearch'

describe('useConversationSearch', () => {
  it('discards a result after reset', async () => {
    let resolve!: (value: never[]) => void
    const api = { searchSessionHits: vi.fn(() => new Promise<never[]>((done) => { resolve = done })) } as unknown as DesktopApi
    const selected = ref({ id: 'session' } as never)
    const search = useConversationSearch(selected, ref('needle'), api)
    const pending = search.load()
    search.reset()
    resolve([])
    await pending
    expect(search.hits.value).toEqual([])
    expect(search.index.value).toBe(-1)
  })
})
