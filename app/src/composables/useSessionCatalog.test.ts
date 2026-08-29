import { describe, expect, it, vi } from 'vitest'
import type { DesktopApi } from '../desktop-api'
import { useSessionCatalog } from './useSessionCatalog'

function fakeApi(searchSessions: DesktopApi['searchSessions']): DesktopApi {
  return { searchSessions } as DesktopApi
}

describe('useSessionCatalog', () => {
  it('owns query pagination and bound date filters', async () => {
    const searchSessions = vi.fn().mockResolvedValue({ sessions: [], total: 0, search_mode: 'hybrid', semantic_status: 'ready' })
    const catalog = useSessionCatalog(fakeApi(searchSessions))
    catalog.query.value = ' test '
    catalog.platform.value = 'deepseek'
    catalog.dateFrom.value = '2026-07-01'
    await catalog.loadSessions()
    expect(searchSessions).toHaveBeenCalledWith(expect.objectContaining({
      q: 'test', platform: 'deepseek', limit: 100, offset: 0, mode: 'hybrid',
      date_from: String(new Date('2026-07-01T00:00:00').getTime() / 1000),
    }))
  })

  it('notifies the coordinator after a reset result', async () => {
    const invalidated = vi.fn()
    const api = fakeApi(vi.fn().mockResolvedValue({ sessions: [{ id: 'visible' }], total: 1, search_mode: 'hybrid', semantic_status: 'ready' }))
    const catalog = useSessionCatalog(api, invalidated)
    await catalog.loadSessions()
    expect([...invalidated.mock.calls[0][0]]).toEqual(['visible'])
  })

  it('reset passes the full previously loaded catalog as visibleIds, not just the first page', async () => {
    // loadMore loads page 2 and a user may select a session there. A reset
    // (refresh / platform change / filter change) must not collapse the
    // visible set to the freshly fetched first page, or App.vue would
    // wrongly clear a selection that still lives on a later loaded page.
    const page1 = Array.from({ length: 100 }, (_, i) => ({ id: `p1-${i}` }))
    const page2 = Array.from({ length: 100 }, (_, i) => ({ id: `p2-${i}` }))
    let call = 0
    const searchSessions = vi.fn().mockImplementation(async () => {
      call += 1
      // first call -> page 1; loadMore -> page 2; reset -> page 1 again
      const sessions = call === 2 ? page2 : page1
      return { sessions, total: 200, search_mode: 'hybrid', semantic_status: 'ready' }
    })
    const invalidated = vi.fn()
    const catalog = useSessionCatalog(fakeApi(searchSessions), invalidated)

    await catalog.loadSessions() // page 1
    await catalog.loadMore() // page 2 -> sessions.value holds all 200
    expect(catalog.sessions.value.map((s) => s.id)).toHaveLength(200)

    invalidated.mockClear()
    await catalog.loadSessions() // reset

    expect(invalidated).toHaveBeenCalledTimes(1)
    const visibleIds = invalidated.mock.calls[0][0] as Set<string>
    // The previously loaded page 2 ids (still valid in the cached catalog)
    // must be present so a selection on a later page is not dropped.
    expect(visibleIds.has('p2-0')).toBe(true)
    expect(visibleIds.has('p2-99')).toBe(true)
    expect(visibleIds.has('p1-0')).toBe(true)
  })
})
