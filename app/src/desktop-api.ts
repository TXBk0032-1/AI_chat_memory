
import { invoke } from '@tauri-apps/api/core'
import type { BranchOverview, Message, SearchHit, SessionOpen, SessionSummary } from './conversation'
import type { LanguagePreference, SupportedLocale } from './i18n/locale'

export type { LanguagePreference, SupportedLocale } from './i18n/locale'

export type ThemePreference = 'system' | 'light' | 'dark'
export type CloseBehavior = 'ask' | 'hide_to_tray' | 'exit'
export type TrayClickBehavior = 'show_menu' | 'open_window' | 'no_action'
export type SearchMode = 'keyword' | 'semantic' | 'hybrid'
export type SemanticStatus = 'disabled' | 'ready' | 'indexing' | 'unavailable'
export type EmbeddingBackendKind = 'local' | 'ollama' | 'llama_cpp' | 'openai_compatible'
export type LocalEmbeddingDevice = 'auto' | 'cuda' | 'cpu'
export type LocalEmbeddingDType = 'auto' | 'f16' | 'f32'

export type RemoteEmbeddingSettings = {
  base_url: string
  api_key?: string
  model: string
  dimensions?: number
}

export type SemanticSearchSettings = {
  enabled: boolean
  default_mode: SearchMode
  backend: EmbeddingBackendKind
  local: {
    model: string
    model_path?: string
    device?: LocalEmbeddingDevice
    dtype?: LocalEmbeddingDType
  }
  ollama: RemoteEmbeddingSettings
  llama_cpp: RemoteEmbeddingSettings
  openai_compatible: RemoteEmbeddingSettings
}

import type { ThemeDefinition } from './theme/types'

export type SettingsModel = {
  setup_complete: boolean
  secret_enabled: boolean
  secret?: string
  allowed_origins: string[]
  data_directory?: string
  close_behavior: CloseBehavior
  tray_click_behavior: TrayClickBehavior
  theme: ThemePreference
  light_theme_id?: string
  dark_theme_id?: string
  language: LanguagePreference
  semantic_search: SemanticSearchSettings
  mcp_enabled: boolean
  cloud_sync: CloudSyncSettings
  custom_themes?: ThemeDefinition[]
}

export type CloudBackendKind = 'webdav' | 's3'
export type S3CloudSyncSettings = {
  endpoint_url: string
  region: string
  bucket: string
  prefix: string
  force_path_style: boolean
}
export type CloudSyncSettings = {
  backend: CloudBackendKind
  enabled: boolean
  connection_verified: boolean
  base_url: string
  root_path: string
  username: string
  encryption_enabled: boolean
  s3: S3CloudSyncSettings
  remote_id: string
  vault_id: string
  generation_id: string
}
export type CloudSyncState = 'disabled' | 'idle' | 'syncing' | 'offline' | 'needs_unlock' | 'auth_error' | 'protocol_error'
export type RemoteDeviceStatus = { device_id: string; display_name: string; last_seen_at?: string | null }
export type CloudSyncStatus = { state: CloudSyncState; last_success_at?: string | null; pending_mutations: number; last_error_code?: string | null; last_error_message?: string | null; devices: RemoteDeviceStatus[] }
export type CloudCredentialInput =
  | { backend: 'webdav'; password: string; sync_password?: string | null }
  | { backend: 's3'; access_key_id: string; secret_access_key: string; session_token?: string | null; sync_password?: string | null }
export type CloudConnectionTestResult = { ok: boolean; message: string; supports_conditional_write: boolean; cloud_sync: CloudSyncSettings }

export type LocalServiceStatus =
  | { state: 'starting' }
  | { state: 'running' }
  | { state: 'stopped' }
  | { state: 'failed'; message?: string }

export type ApiStatus = {
  service: { state: string; message?: string }
  userscript_connected: boolean
  last_userscript_request_at?: number
  mcp: LocalServiceStatus
  mcp_url: string
}

export type SemanticRuntimeStatus = {
  enabled: boolean
  status: SemanticStatus
  backend: EmbeddingBackendKind
  model_id: string
  dimensions?: number
  pending_chunks: number
  ready_chunks: number
  message?: string
  local_model_ready: boolean
  local_model_path?: string
  device?: string
  dtype?: string
  reindex?: ReindexProgress | null
}

export type ModelDownloadProgress = {
  stage: string
  file?: string
  file_index: number
  file_count: number
  downloaded_bytes: number
  total_bytes?: number
  fraction: number
  message: string
}

export type ReindexProgress = {
  stage: string
  total_sessions: number
  processed_sessions: number
  total_chunks: number
  ready_chunks: number
  pending_chunks: number
  fraction: number
  message: string
}

export type EmbeddingHealth = {
  ok: boolean
  backend: EmbeddingBackendKind
  model_id: string
  dimensions?: number
  message: string
}

export type SessionSearchQuery = {
  q: string | null
  platform: string | null
  date_from: string | null
  date_to: string | null
  limit: number
  offset: number
  mode?: SearchMode | null
}

export type SessionListResult = {
  sessions: SessionSummary[]
  total: number
  search_mode: SearchMode
  semantic_status: SemanticStatus
}

export type ExportFilePayload = { encoding: 'utf8' | 'base64'; data: string }

export interface DesktopApi {
  searchSessions(query: SessionSearchQuery): Promise<SessionListResult>
  openSession(id: string, anchorSeq: number | null): Promise<SessionOpen>
  getSessionMessages(id: string, startSeq: number, limit: number): Promise<Message[]>
  searchSessionHits(id: string, query: string, mode?: SearchMode | null): Promise<SearchHit[]>
  getSessionBranches(id: string): Promise<BranchOverview>
  deleteSession(id: string): Promise<void>
  importDeepseekZip(path: string): Promise<void>
  getSettings(): Promise<SettingsModel>
  saveSettings(settings: SettingsModel, cloudSyncCredentials?: CloudCredentialInput | null): Promise<SettingsModel>
  rotateSecret(): Promise<SettingsModel>
  getApiStatus(): Promise<ApiStatus>
  getSemanticStatus(): Promise<SemanticRuntimeStatus>
  checkEmbeddingBackend(): Promise<EmbeddingHealth>
  reindexSemanticSearch(): Promise<number>
  downloadLocalEmbeddingModel(): Promise<void>
  importLocalEmbeddingModel(path: string): Promise<void>
  cancelSemanticWork(): Promise<void>
  moveDataDirectory(path: string): Promise<void>
  confirmCloseBehavior(behavior: Exclude<CloseBehavior, 'ask'>): Promise<void>
  writeExportFile(path: string, payload: ExportFilePayload): Promise<void>
  printToPdf(path: string, options: { compact: boolean }): Promise<void>
  getCloudSyncStatus(): Promise<CloudSyncStatus>
  testCloudSyncConnection(cloudSync: CloudSyncSettings, credentials: CloudCredentialInput): Promise<CloudConnectionTestResult>
  syncNow(): Promise<CloudSyncStatus>
  rewriteCloudArchive(): Promise<CloudSyncStatus>
  removeCloudDeviceRecord(deviceId: string): Promise<CloudSyncStatus>
  setNativeLocale(locale: SupportedLocale): Promise<void>
}

export const desktopApi: DesktopApi = {
  searchSessions: (query) => invoke('search_sessions', { query }),
  openSession: (id, anchorSeq) => invoke('open_session', { id, anchorSeq }),
  getSessionMessages: (id, startSeq, limit) => invoke('get_session_messages', { id, startSeq, limit }),
  searchSessionHits: (id, query, mode = null) => invoke('search_session_hits', { id, query, mode }),
  getSessionBranches: (id) => invoke('get_session_branches', { id }),
  deleteSession: (id) => invoke('delete_session', { id }),
  importDeepseekZip: (path) => invoke('import_deepseek_zip', { path }),
  getSettings: () => invoke('get_settings'),
  saveSettings: (settings, cloudSyncCredentials = null) => invoke('save_settings', { settings, cloudSyncCredentials }),
  rotateSecret: () => invoke('rotate_secret'),
  getApiStatus: () => invoke('get_api_status'),
  getSemanticStatus: () => invoke('get_semantic_status'),
  checkEmbeddingBackend: () => invoke('check_embedding_backend'),
  reindexSemanticSearch: () => invoke('reindex_semantic_search'),
  downloadLocalEmbeddingModel: () => invoke('download_local_embedding_model'),
  importLocalEmbeddingModel: (path) => invoke('import_local_embedding_model', { path }),
  cancelSemanticWork: () => invoke('cancel_semantic_work'),
  moveDataDirectory: (path) => invoke('move_data_directory', { path }),
  confirmCloseBehavior: (behavior) => invoke('confirm_close_behavior', { behavior }),
  writeExportFile: (path, payload) => invoke('write_export_file', { path, payload }),
  printToPdf: (path, options) => invoke('print_to_pdf', { path, compact: options.compact }),
  getCloudSyncStatus: () => invoke('get_cloud_sync_status'),
  testCloudSyncConnection: (cloudSync, credentials) => invoke('test_cloud_sync_connection', { cloudSync, credentials }),
  syncNow: () => invoke('sync_now'),
  rewriteCloudArchive: () => invoke('rewrite_cloud_archive'),
  removeCloudDeviceRecord: (deviceId) => invoke('remove_cloud_device_record', { deviceId }),
  setNativeLocale: (locale) => invoke('set_native_locale', { locale }),
}
