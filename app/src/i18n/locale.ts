export type SupportedLocale = 'zh-CN' | 'en-US'
export type LanguagePreference = 'system' | SupportedLocale

export const supportedLocales = ['zh-CN', 'en-US'] as const

export function browserLanguages(): readonly string[] {
  if (typeof navigator === 'undefined') return []
  return navigator.languages?.length ? navigator.languages : [navigator.language]
}

export function resolveLocale(
  preference: LanguagePreference,
  languages: readonly string[] = browserLanguages(),
): SupportedLocale {
  if (preference === 'zh-CN' || preference === 'en-US') return preference
  return languages.some((language) => /^zh(?:-|$)/i.test(language)) ? 'zh-CN' : 'en-US'
}

function asDate(value: Date | string | number): Date {
  if (value instanceof Date) return value
  const numeric = typeof value === 'number' ? value : Number(value)
  if (Number.isFinite(numeric)) return new Date(Math.abs(numeric) < 1_000_000_000_000 ? numeric * 1000 : numeric)
  return new Date(value)
}

export function formatDate(
  value: Date | string | number,
  locale: SupportedLocale,
  compact = false,
): string {
  const date = asDate(value)
  if (Number.isNaN(date.valueOf())) return String(value)
  return new Intl.DateTimeFormat(locale, compact
    ? { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }
    : { year: 'numeric', month: 'long', day: 'numeric', hour: '2-digit', minute: '2-digit' }
  ).format(date)
}
