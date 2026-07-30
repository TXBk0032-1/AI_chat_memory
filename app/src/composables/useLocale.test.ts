/** @vitest-environment happy-dom */

import { describe, expect, it, vi } from 'vitest'
import { currentLocale, setLocale } from '../i18n'
import { useLocale } from './useLocale'

describe('language preview lifecycle', () => {
  it('previews immediately and restores the opening locale on cancel', async () => {
    setLocale('zh-CN')
    const syncNative = vi.fn().mockResolvedValue(undefined)
    const locale = useLocale(syncNative, ['en-US'])

    locale.beginPreview()
    await locale.previewLanguage('en-US')
    expect(currentLocale()).toBe('en-US')
    expect(syncNative).toHaveBeenLastCalledWith('en-US')

    await locale.cancelPreview()
    expect(currentLocale()).toBe('zh-CN')
    expect(syncNative).toHaveBeenLastCalledWith('zh-CN')
  })

  it('keeps the previewed locale after save accepts it', async () => {
    setLocale('zh-CN')
    const syncNative = vi.fn().mockResolvedValue(undefined)
    const locale = useLocale(syncNative, ['en-US'])

    locale.beginPreview()
    await locale.previewLanguage('system')
    await locale.acceptPreview()
    await locale.cancelPreview()

    expect(currentLocale()).toBe('en-US')
    expect(syncNative).toHaveBeenNthCalledWith(2, 'en-US')
  })
})
