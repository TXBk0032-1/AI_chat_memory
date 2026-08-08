import {
  currentLocale,
  setLanguagePreference,
  setLocale,
  type LanguagePreference,
  type SupportedLocale,
} from '../i18n'

export type NativeLocaleSync = (locale: SupportedLocale) => Promise<void>

export function useLocale(
  syncNative: NativeLocaleSync,
  languages?: readonly string[],
) {
  let previewOrigin: SupportedLocale | null = null

  function beginPreview() {
    previewOrigin = currentLocale()
  }

  async function previewLanguage(preference: LanguagePreference) {
    const locale = setLanguagePreference(preference, languages)
    await syncNative(locale)
    return locale
  }

  async function acceptPreview() {
    previewOrigin = null
    const locale = currentLocale()
    await syncNative(locale)
    return locale
  }

  async function cancelPreview() {
    if (!previewOrigin) return currentLocale()
    const locale = previewOrigin
    previewOrigin = null
    setLocale(locale)
    await syncNative(locale)
    return locale
  }

  return { beginPreview, previewLanguage, acceptPreview, cancelPreview }
}
