import type { LanguagePreference, SettingsModel, SupportedLocale } from './desktop-api'

type AppSettingsInitialization = {
  initialSettings?: SettingsModel
  loadSettings(): Promise<SettingsModel>
  applyPreference(preference: LanguagePreference): Promise<SupportedLocale>
  applySettings(settings: SettingsModel): void
}

export async function initializeAppSettings({
  initialSettings,
  loadSettings,
  applyPreference,
  applySettings,
}: AppSettingsInitialization): Promise<SettingsModel> {
  const preloaded = initialSettings !== undefined
  const value = preloaded ? initialSettings : await loadSettings()

  if (preloaded) {
    applySettings(value)
  } else {
    try {
      await applyPreference(value.language)
    } finally {
      applySettings(value)
    }
  }

  return value
}
