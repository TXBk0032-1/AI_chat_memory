import { describe, expect, it } from 'vitest'
import appSource from './App.vue?raw'
import messageBlockSource from './MessageBlock.vue?raw'

describe('clipboard and link failure feedback (FE-13)', () => {
  it('catches context menu copy failures and shows a toast', () => {
    const body = appSource.slice(appSource.indexOf('async function copyContextSelection'), appSource.indexOf('function selectConversationContent'))
    expect(body).toContain('try {')
    expect(body).toContain('await navigator.clipboard.writeText(contextMenu.value.selectedText)')
    expect(body).toContain("catch {")
    expect(body).toContain("showToast(t('app.copyFailed'))")
  })

  it('catches markdown link open failures and shows a toast', () => {
    const body = appSource.slice(appSource.indexOf('async function openMarkdownLink'), appSource.indexOf('async function refreshApiStatus') > -1 ? appSource.indexOf('async function refreshApiStatus') : appSource.indexOf('function formatDate'))
    expect(body).toContain('try {')
    expect(body).toContain('await openUrl(link.href)')
    expect(body).toContain("catch {")
    expect(body).toContain("showToast(t('app.openLinkFailed'))")
  })

  it('catches code copy failures with visible button feedback in MessageBlock', () => {
    const body = messageBlockSource.slice(messageBlockSource.indexOf('function handleBlockClick'), messageBlockSource.indexOf('function notifyRendered'))
    expect(body).toContain('navigator.clipboard.writeText(text).then(')
    // Rejection is handled by the second callback; no floating promise.
    expect(body).toContain('copy-failed')
    expect(body).toContain("t('app.copyFailed')")
    expect(body).not.toMatch(/void navigator\.clipboard\.writeText/)
  })
})
