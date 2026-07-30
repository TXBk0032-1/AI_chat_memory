import { beforeEach, describe, expect, it, vi } from 'vitest'

const invoke = vi.fn()
vi.mock('@tauri-apps/api/core', () => ({ invoke }))

describe('desktopApi', () => {
  beforeEach(() => invoke.mockReset())

  it('maps session operations to stable Tauri commands', async () => {
    invoke.mockResolvedValue({ sessions: [], total: 0, search_mode: 'hybrid', semantic_status: 'ready' })
    const { desktopApi } = await import('./desktop-api')
    const query = { q: null, platform: null, date_from: null, date_to: null, limit: 100, offset: 0, mode: 'hybrid' as const }
    await desktopApi.searchSessions(query)
    expect(invoke).toHaveBeenCalledWith('search_sessions', { query })
    await desktopApi.openSession('session', 42)
    expect(invoke).toHaveBeenLastCalledWith('open_session', { id: 'session', anchorSeq: 42 })
  })

  it('maps semantic maintenance commands', async () => {
    invoke.mockResolvedValue({ ok: true })
    const { desktopApi } = await import('./desktop-api')
    await desktopApi.getSemanticStatus()
    expect(invoke).toHaveBeenCalledWith('get_semantic_status')
    await desktopApi.reindexSemanticSearch()
    expect(invoke).toHaveBeenCalledWith('reindex_semantic_search')
  })

  it('keeps lifecycle payloads stable', async () => {
    invoke.mockResolvedValue(undefined)
    const { desktopApi } = await import('./desktop-api')
    await desktopApi.moveDataDirectory('D:/archive')
    expect(invoke).toHaveBeenLastCalledWith('move_data_directory', { path: 'D:/archive' })
    await desktopApi.confirmCloseBehavior('hide_to_tray')
    expect(invoke).toHaveBeenLastCalledWith('confirm_close_behavior', { behavior: 'hide_to_tray' })
  })

  it('syncs the effective locale through the stable native command', async () => {
    invoke.mockResolvedValue(undefined)
    const { desktopApi } = await import('./desktop-api')

    await desktopApi.setNativeLocale('en-US')

    expect(invoke).toHaveBeenLastCalledWith('set_native_locale', { locale: 'en-US' })
  })
})
