import { describe, expect, it } from 'vitest'
import { createI18n } from 'vue-i18n'
import enUS from './locales/en-US'
import zhCN from './locales/zh-CN'

function leafKeys(value: object, prefix = ''): string[] {
  return Object.entries(value).flatMap(([key, child]) => {
    const path = prefix ? `${prefix}.${key}` : key
    return typeof child === 'string' ? [path] : leafKeys(child, path)
  })
}

describe('desktop message resources', () => {
  it('keeps Chinese and English resource paths identical', () => {
    expect(leafKeys(enUS).sort()).toEqual(leafKeys(zhCN).sort())
  })

  it('resolves count and error interpolation without leftover placeholders', () => {
    const i18n = createI18n({ legacy: false, locale: 'en-US', messages: { 'zh-CN': zhCN, 'en-US': enUS } })
    const count = i18n.global.t('session.messageCount', { count: 3 })
    const failure = i18n.global.t('errors.exportFailed', { reason: 'disk full' })

    expect(count).toContain('3')
    expect(failure).toContain('disk full')
    expect(`${count}${failure}`).not.toMatch(/\{\w+\}/)
  })
})
