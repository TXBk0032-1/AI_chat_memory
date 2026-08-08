import { createI18n } from 'vue-i18n'
import enUS from './locales/en-US'
import zhCN from './locales/zh-CN'
import { resolveLocale, type LanguagePreference, type SupportedLocale } from './locale'

export const i18n = createI18n({
  legacy: false,
  locale: resolveLocale('system'),
  fallbackLocale: 'en-US',
  messages: {
    'zh-CN': zhCN,
    'en-US': enUS,
  },
})

export function currentLocale(): SupportedLocale {
  return i18n.global.locale.value as SupportedLocale
}

export function translate(key: string, named?: Record<string, unknown>): string {
  return named ? i18n.global.t(key, named) : i18n.global.t(key)
}

function applyDocumentLocale(locale: SupportedLocale) {
  if (typeof document === 'undefined') return
  document.documentElement.lang = locale
  document.title = translate('app.title')
  document.querySelectorAll<HTMLElement>('[data-i18n]').forEach((element) => {
    const key = element.dataset.i18n
    if (key) element.textContent = translate(key)
  })
}

export function setLocale(locale: SupportedLocale): SupportedLocale {
  i18n.global.locale.value = locale
  applyDocumentLocale(locale)
  return locale
}

export function setLanguagePreference(
  preference: LanguagePreference,
  languages?: readonly string[],
): SupportedLocale {
  return setLocale(resolveLocale(preference, languages))
}

type LocaleSettings = { language?: LanguagePreference }
type LocaleInitializationApi<TSettings extends LocaleSettings = LocaleSettings> = {
  getSettings(): Promise<TSettings>
  setNativeLocale(locale: SupportedLocale): Promise<void>
}

async function loadLocale<TSettings extends LocaleSettings>(
  api: LocaleInitializationApi<TSettings>,
  languages?: readonly string[],
): Promise<{ locale: SupportedLocale; settings?: TSettings }> {
  let preference: LanguagePreference = 'system'
  let settings: TSettings | undefined
  try {
    settings = await api.getSettings()
    if (settings.language === 'zh-CN' || settings.language === 'en-US' || settings.language === 'system') {
      preference = settings.language
    }
  } catch {
    // Reading settings must never prevent the WebView from mounting.
  }
  const locale = setLanguagePreference(preference, languages)
  try {
    await api.setNativeLocale(locale)
  } catch {
    // Native UI synchronization is best-effort during startup.
  }
  return { locale, settings }
}

export async function initializeLocale<TSettings extends LocaleSettings>(
  api: LocaleInitializationApi<TSettings>,
  languages?: readonly string[],
): Promise<SupportedLocale> {
  return (await loadLocale(api, languages)).locale
}

export async function initializeLocaleAndMount<TSettings extends LocaleSettings>(
  api: LocaleInitializationApi<TSettings>,
  mount: (settings?: TSettings) => void,
  languages?: readonly string[],
): Promise<SupportedLocale> {
  const { locale, settings } = await loadLocale(api, languages)
  mount(settings)
  return locale
}

export type { LanguagePreference, SupportedLocale } from './locale'
