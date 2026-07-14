import { invoke } from '@tauri-apps/api/core'
import type { BranchOverview, Message, SearchHit, SessionOpen, SessionSummary } from './conversation'

export type ThemePreference = 'system' | 'light' | 'dark'
export type CloseBehavior = 'ask' | 'hide_to_tray' | 'exit'
export type TrayClickBehavior = 'show_menu' | 'open_window' | 'no_action'
export type SettingsModel = {
  setup_complete: boolean
  secret_enabled: boolean
  secret?: string
  allowed_origins: string[]
  data_directory?: string
  close_behavior: CloseBehavior
  tray_click_behavior: TrayClickBehavior
  theme: ThemePreference
}
export type ApiStatus = {
  service: { state: string; message?: string }
  userscript_connected: boolean
  last_userscript_request_at?: number
}
export type SessionSearchQuery = {
  q: string | null
  platform: string | null
  date_from: string | null
  date_to: string | null
  limit: number
  offset: number
}

export interface DesktopApi {
  searchSessions(query: SessionSearchQuery): Promise<{ sessions: SessionSummary[]; total: number }>
  openSession(id: string, anchorSeq: number | null): Promise<SessionOpen>
  getSessionMessages(id: string, startSeq: number, limit: number): Promise<Message[]>
  searchSessionHits(id: string, query: string): Promise<SearchHit[]>
  getSessionBranches(id: string): Promise<BranchOverview>
  deleteSession(id: string): Promise<void>
  importDeepseekZip(path: string): Promise<void>
  getSettings(): Promise<SettingsModel>
  saveSettings(settings: SettingsModel): Promise<SettingsModel>
  rotateSecret(): Promise<SettingsModel>
  getApiStatus(): Promise<ApiStatus>
  moveDataDirectory(path: string): Promise<void>
  confirmCloseBehavior(behavior: Exclude<CloseBehavior, 'ask'>): Promise<void>
}

export const desktopApi: DesktopApi = {
  searchSessions: (query) => invoke('search_sessions', { query }),
  openSession: (id, anchorSeq) => invoke('open_session', { id, anchorSeq }),
  getSessionMessages: (id, startSeq, limit) => invoke('get_session_messages', { id, startSeq, limit }),
  searchSessionHits: (id, query) => invoke('search_session_hits', { id, query }),
  getSessionBranches: (id) => invoke('get_session_branches', { id }),
  deleteSession: (id) => invoke('delete_session', { id }),
  importDeepseekZip: (path) => invoke('import_deepseek_zip', { path }),
  getSettings: () => invoke('get_settings'),
  saveSettings: (settings) => invoke('save_settings', { settings }),
  rotateSecret: () => invoke('rotate_secret'),
  getApiStatus: () => invoke('get_api_status'),
  moveDataDirectory: (path) => invoke('move_data_directory', { path }),
  confirmCloseBehavior: (behavior) => invoke('confirm_close_behavior', { behavior }),
}
