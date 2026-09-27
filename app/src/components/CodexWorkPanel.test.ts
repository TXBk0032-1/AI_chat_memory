/** @vitest-environment happy-dom */

import { createApp, defineComponent, h, nextTick, ref } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { WorkItem } from '../conversation'
import CodexWorkPanel from './CodexWorkPanel.vue'
import { setLocale } from '../i18n'

function mount(items: WorkItem[]) {
  document.body.innerHTML = '<div id="app"></div>'
  const openSubagent = vi.fn()
  const Root = defineComponent({
    setup: () => () => h(CodexWorkPanel, { items, onOpenSubagent: openSubagent }),
  })
  const app = createApp(Root)
  app.mount(document.getElementById('app')!)
  return { root: document.body, openSubagent, unmount: () => { app.unmount(); document.body.innerHTML = '' } }
}

function item(overrides: Partial<WorkItem> = {}): WorkItem {
  return { work_seq: 0, seq: 0, kind: 'command', title: 'ls -la', expandable: true, truncated: false, ...overrides }
}

beforeEach(() => setLocale('zh-CN'))
afterEach(() => { vi.restoreAllMocks(); document.body.innerHTML = '' })

describe('CodexWorkPanel', () => {
  it('renders an encrypted reasoning item as a non-expandable single line', () => {
    const h = mount([item({ kind: 'reasoning', title: '推理摘要', body: undefined, expandable: false })])
    try {
      const line = h.root.querySelector('.work-item.reasoning.locked')
      expect(line?.textContent).toContain('推理摘要')
      expect(h.root.querySelector('.work-item.reasoning .work-item-body')).toBeNull()
    } finally { h.unmount() }
  })

  it('expands a command item body on click', async () => {
    const h = mount([item({ kind: 'command', title: 'ls', body: 'total 0', expandable: true })])
    try {
      const toggle = h.root.querySelector<HTMLButtonElement>('.work-item.command .work-item-toggle')!
      toggle.click()
      await nextTick()
      expect(h.root.querySelector('.work-item.command .work-item-body')?.textContent).toContain('total 0')
    } finally { h.unmount() }
  })

  it('emits openSubagent with the agent thread id', () => {
    const h = mount([item({ kind: 'subagent', title: '子代理', agent_thread_id: 'sub-1', agent_label: '研究员', expandable: false })])
    try {
      h.root.querySelector<HTMLButtonElement>('.work-item.subagent .work-item-toggle')!.click()
      expect(h.openSubagent).toHaveBeenCalledWith('sub-1')
    } finally { h.unmount() }
  })

  it('shows a truncation notice when an item body is truncated', async () => {
    const h = mount([item({ kind: 'file_change', title: 'src/a.rs', body: 'diff…', expandable: true, truncated: true })])
    try {
      h.root.querySelector<HTMLButtonElement>('.work-item.file_change .work-item-toggle')!.click()
      await nextTick()
      expect(h.root.querySelector('.work-item-truncated')?.textContent).toContain('已截断')
    } finally { h.unmount() }
  })
})
