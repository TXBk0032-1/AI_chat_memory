/** @vitest-environment happy-dom */

import { describe, expect, it } from 'vitest'
import { formatDate, resolveLocale } from './locale'
import { setLocale } from './index'

describe('locale resolution', () => {
  it('keeps an explicit supported locale regardless of browser languages', () => {
    expect(resolveLocale('zh-CN', ['en-US'])).toBe('zh-CN')
    expect(resolveLocale('en-US', ['zh-CN'])).toBe('en-US')
  })

  it('maps Chinese system variants to Simplified Chinese', () => {
    expect(resolveLocale('system', ['zh-Hans-CN'])).toBe('zh-CN')
    expect(resolveLocale('system', ['fr-FR', 'zh-TW'])).toBe('zh-CN')
  })

  it('falls back to English for unsupported and empty system languages', () => {
    expect(resolveLocale('system', ['fr-FR'])).toBe('en-US')
    expect(resolveLocale('system', [])).toBe('en-US')
  })
})

describe('locale application', () => {
  it('updates vue-i18n, HTML language, title, and stable boot copy together', () => {
    document.body.innerHTML = `
      <span data-i18n="app.title"></span>
      <span data-i18n="boot.heading"></span>
      <span data-i18n="boot.subtitle"></span>
      <span data-i18n="boot.status"></span>
    `

    setLocale('en-US')

    expect(document.documentElement.lang).toBe('en-US')
    expect(document.title).toBe('Conversation Archive')
    expect(document.querySelector('[data-i18n="boot.heading"]')?.textContent).toBe('All conversations')
    expect(document.querySelector('[data-i18n="boot.status"]')?.textContent).toBe('Starting…')
  })
})

describe('localized dates', () => {
  it('formats the same instant differently for each effective locale', () => {
    const instant = new Date('2026-07-30T08:15:00Z')
    const chinese = formatDate(instant, 'zh-CN')
    const english = formatDate(instant, 'en-US')

    expect(chinese).not.toBe(english)
    expect(chinese).toMatch(/2026/)
    expect(chinese).toMatch(/[年月]/)
    expect(english).toMatch(/2026/)
    expect(english).toMatch(/Jul|July/)
  })
})
