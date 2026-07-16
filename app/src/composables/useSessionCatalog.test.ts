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
})
