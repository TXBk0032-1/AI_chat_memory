import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { SettingsModel } from './desktop-api'

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

  it('tests cloud sync with draft S3 settings and tagged credentials', async () => {
    invoke.mockResolvedValue({ ok: true })
    const { desktopApi } = await import('./desktop-api')
    const cloudSync = {
      backend: 's3' as const,
      enabled: true,
      connection_verified: false,
      base_url: '',
      root_path: '',
      username: '',
      encryption_enabled: true,
      s3: {
        endpoint_url: 'http://127.0.0.1:9000',
        region: 'us-east-1',
        bucket: 'archive',
        prefix: 'team/chat',
        force_path_style: true,
      },
      remote_id: 'remote-a',
      vault_id: 'vault-a',
      generation_id: 'generation-a',
    }
    const credentials = {
      backend: 's3' as const,
      access_key_id: 'AKID',
      secret_access_key: 'secret',
      session_token: 'token',
      sync_password: 'sync',
    }

    await desktopApi.testCloudSyncConnection(cloudSync, credentials)

    expect(invoke).toHaveBeenLastCalledWith('test_cloud_sync_connection', {
      cloudSync,
      credentials,
    })
  })

  it('saves settings and cloud credentials in one typed command', async () => {
    invoke.mockResolvedValue({ setup_complete: true })
    const { desktopApi } = await import('./desktop-api')
    const settings: SettingsModel = {
      setup_complete: true,
      secret_enabled: false,
      allowed_origins: [],
      close_behavior: 'ask',
      tray_click_behavior: 'show_menu',
      theme: 'system',
      language: 'system',
      semantic_search: {
        enabled: true,
        default_mode: 'hybrid',
        backend: 'local',
        local: { model: 'test', device: 'auto', dtype: 'auto' },
        ollama: { base_url: 'http://127.0.0.1:11434', model: 'test' },
        llama_cpp: { base_url: 'http://127.0.0.1:8080/v1', model: 'test' },
        openai_compatible: { base_url: 'https://example.test/v1', model: 'test' },
      },
      mcp_enabled: true,
      cloud_sync: {
        backend: 's3',
        enabled: true,
        connection_verified: true,
        base_url: '',
        root_path: '',
        username: '',
        encryption_enabled: true,
        s3: {
          endpoint_url: 'http://127.0.0.1:9000',
          region: 'us-east-1',
          bucket: 'archive',
          prefix: 'team/chat',
          force_path_style: true,
        },
        remote_id: 'remote-a',
        vault_id: 'vault-a',
        generation_id: 'generation-a',
      },
    }
    const credentials = {
      backend: 's3' as const,
      access_key_id: 'AKID',
      secret_access_key: 'secret',
      session_token: null,
      sync_password: 'sync',
    }

    await desktopApi.saveSettings(settings, credentials)

    expect(invoke).toHaveBeenLastCalledWith('save_settings', {
      settings,
      cloudSyncCredentials: credentials,
    })
  })

  it('syncs the effective locale through the stable native command', async () => {
    invoke.mockResolvedValue(undefined)
    const { desktopApi } = await import('./desktop-api')

    await desktopApi.setNativeLocale('en-US')

    expect(invoke).toHaveBeenLastCalledWith('set_native_locale', { locale: 'en-US' })
  })
})

