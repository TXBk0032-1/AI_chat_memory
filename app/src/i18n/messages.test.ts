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

  it('contains key first-run surfaces in both languages', () => {
    expect(zhCN.app.title).toBe('对话归档')
    expect(enUS.app.title).toBe('Conversation Archive')
    expect(zhCN.settings.language.title).toBe('界面语言')
    expect(enUS.settings.language.title).toBe('Display language')
    expect(zhCN.export.dialogTitle).toBe('导出聊天记录')
    expect(enUS.export.dialogTitle).toBe('Export conversation')
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
