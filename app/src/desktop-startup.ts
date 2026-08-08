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
