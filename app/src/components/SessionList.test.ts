/** @vitest-environment happy-dom */

import { afterEach, describe, expect, it, vi } from 'vitest'
import { createApp, defineComponent, h, nextTick } from 'vue'
import SessionList from './SessionList.vue'
import { setLocale } from '../i18n'
import type { SessionSummary } from '../conversation'

function summary(over: Partial<SessionSummary>): SessionSummary {
  return { id: 'x', platform: 'codex', platform_session_id: 't', title: 'T', updated_at: '2026-09-27T00:00:00Z', message_count: 1, ...over }
}

async function mountList(props: Record<string, unknown>) {
  document.body.innerHTML = '<div id="app"></div>'
  const Root = defineComponent({ setup: () => () => h(SessionList, props) })
  const app = createApp(Root)
  app.mount(document.getElementById('app')!)
  await nextTick()
  return app
}

describe('SessionList project column and children', () => {
  setLocale('zh-CN')
  afterEach(() => { document.body.innerHTML = '' })

  it('renders the project cell', async () => {
    const app = await mountList({ sessions: [summary({ project: 'my-repo' })], total: 1, loading: false, filtered: false, query: '', childSessions: new Map(), expandedParents: new Set() })
    try { expect(document.querySelector('.project-cell')?.textContent).toContain('my-repo') }
    finally { app.unmount() }
  })

  it('emits toggleChildren when the collapse arrow is clicked', async () => {
    const onToggle = vi.fn()
    const app = await mountList({ sessions: [summary({ id: 'p', child_count: 2 })], total: 1, loading: false, filtered: false, query: '', childSessions: new Map(), expandedParents: new Set(), onToggleChildren: onToggle })
    try {
      const arrow = document.querySelector('.child-toggle') as HTMLElement
      expect(arrow).toBeTruthy()
      arrow.click()
      expect(onToggle).toHaveBeenCalledWith('p')
    } finally { app.unmount() }
  })

  it('renders child rows when the parent is expanded', async () => {
    const children = new Map([['p', [summary({ id: 'c', title: '子会话' })]]])
    const app = await mountList({ sessions: [summary({ id: 'p', child_count: 1 })], total: 1, loading: false, filtered: false, query: '', childSessions: children, expandedParents: new Set(['p']) })
    try { expect(document.querySelector('.session-row.child')?.textContent).toContain('子会话') }
    finally { app.unmount() }
  })
})
