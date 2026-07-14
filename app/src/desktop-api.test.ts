import { beforeEach, describe, expect, it, vi } from 'vitest'

const invoke = vi.fn()
vi.mock('@tauri-apps/api/core', () => ({ invoke }))

describe('desktopApi', () => {
  beforeEach(() => invoke.mockReset())

  it('maps session operations to stable Tauri commands', async () => {
    invoke.mockResolvedValue({ sessions: [], total: 0 })
    const { desktopApi } = await import('./desktop-api')
    const query = { q: null, platform: null, date_from: null, date_to: null, limit: 100, offset: 0 }
    await desktopApi.searchSessions(query)
    expect(invoke).toHaveBeenCalledWith('search_sessions', { query })
    await desktopApi.openSession('session', 42)
    expect(invoke).toHaveBeenLastCalledWith('open_session', { id: 'session', anchorSeq: 42 })
  })

  it('keeps lifecycle payloads stable', async () => {
    invoke.mockResolvedValue(undefined)
    const { desktopApi } = await import('./desktop-api')
    await desktopApi.moveDataDirectory('D:/archive')
    expect(invoke).toHaveBeenLastCalledWith('move_data_directory', { path: 'D:/archive' })
    await desktopApi.confirmCloseBehavior('hide_to_tray')
    expect(invoke).toHaveBeenLastCalledWith('confirm_close_behavior', { behavior: 'hide_to_tray' })
  })
})
