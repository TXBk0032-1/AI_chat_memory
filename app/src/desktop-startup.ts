import type { UnlistenFn } from '@tauri-apps/api/event'
import type { DesktopApi, SettingsModel, SupportedLocale } from './desktop-api'
import { initializeLocaleAndMount } from './i18n'

type StartupApi = Pick<DesktopApi, 'getSettings' | 'setNativeLocale'>
type DesktopMount = (settings?: SettingsModel) => void

export function startDesktopApp(
  api: StartupApi,
  mount: DesktopMount,
  languages?: readonly string[],
): Promise<SupportedLocale> {
  return initializeLocaleAndMount(api, mount, languages)
}

export type AppStartupSteps = {
  /** Resolves once applySettings has installed the persisted settings. */
  settingsReady: Promise<unknown>
  /** Loads the first session list page into the shell. */
  loadSessions(): Promise<unknown> | unknown
  /** Subscribes the close-behavior event; may reject. */
  subscribeCloseBehavior(): Promise<UnlistenFn>
  /** Refreshes the API service status once. */
  refreshApiStatus(): Promise<unknown>
  /** Starts the recurring API status poll. */
  startStatusPolling(): void
}

export async function runAppStartup(steps: AppStartupSteps): Promise<UnlistenFn | undefined> {
  // The first catalog load must observe the persisted search mode installed by
  // applySettings, so it starts only after the settings promise settles. A
  // failed settings load must still populate the shell with the defaults.
  const sessionsReady = steps.settingsReady.then(steps.loadSessions, steps.loadSessions)
  let unlisten: UnlistenFn | undefined
  try {
    unlisten = await steps.subscribeCloseBehavior()
  } catch (reason) {
    // A failed event subscription must not block the remaining startup steps
    // (settings, session list, API status polling).
    console.error('[STARTUP] Failed to subscribe to close-behavior-requested:', reason)
  }
  await Promise.allSettled([steps.settingsReady, steps.refreshApiStatus(), sessionsReady])
  steps.startStatusPolling()
  return unlisten
}
