import type { SettingsModel } from '../desktop-api'

// A recursive partial that lets callers override any nested field (e.g.
// `cloud_sync.s3.bucket`) without having to restate sibling fields.
type DeepPartial<T> = {
  [K in keyof T]?: T[K] extends (infer U)[]
    ? U[]
    : T[K] extends object | undefined
      ? DeepPartial<T[K]>
      : T[K]
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function mergeDeep<T>(base: T, overrides?: DeepPartial<T>): T {
  if (!overrides) return base
  const result: Record<string, unknown> = { ...(base as Record<string, unknown>) }
  for (const key of Object.keys(overrides)) {
    const overrideValue = (overrides as Record<string, unknown>)[key]
    const baseValue = (base as Record<string, unknown>)[key]
    result[key] = isPlainObject(overrideValue) && isPlainObject(baseValue)
      ? mergeDeep(baseValue, overrideValue as DeepPartial<typeof baseValue>)
      : overrideValue
  }
  return result as T
}

// A fresh base fixture per call so nested objects (semantic_search,
// cloud_sync, ...) are never shared by reference across tests, even when no
// overrides touch them.
function baseSettingsFixture(): SettingsModel {
  return {
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
      ollama: { base_url: '', model: 'test' },
      llama_cpp: { base_url: '', model: 'test' },
      openai_compatible: { base_url: '', model: 'test' },
    },
    mcp_enabled: true,
    codex: { auto_watch: false },
    cloud_sync: {
      backend: 'webdav',
      enabled: false,
      connection_verified: false,
      base_url: '',
      root_path: '',
      username: '',
      encryption_enabled: false,
      s3: { endpoint_url: '', region: 'us-east-1', bucket: '', prefix: '', force_path_style: false },
      remote_id: 'default',
      vault_id: 'default',
      generation_id: 'generation-1',
    },
  }
}

/**
 * Builds a `SettingsModel` fixture for tests. Pass a `DeepPartial<SettingsModel>`
 * to override any field (nested objects are merged, not replaced) — e.g.
 * `createSettingsFixture({ language: 'zh-CN', cloud_sync: { backend: 's3' } })`.
 */
export function createSettingsFixture(overrides?: DeepPartial<SettingsModel>): SettingsModel {
  return mergeDeep(baseSettingsFixture(), overrides)
}
