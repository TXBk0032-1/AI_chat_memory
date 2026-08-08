/** @vitest-environment happy-dom */

import { createApp, defineComponent, h } from 'vue'
import { afterEach, describe, expect, it } from 'vitest'
import ExportDialog from '../components/ExportDialog.vue'
import SessionList from '../components/SessionList.vue'
import { setLocale } from './index'

const mountedApps: Array<ReturnType<typeof createApp>> = []

function mount(component: Parameters<typeof h>[0], props: Record<string, unknown>) {
  document.body.innerHTML = '<div id="app"></div>'
  const Root = defineComponent({ setup: () => () => h(component, props) })
  const app = createApp(Root)
  app.mount(document.querySelector('#app')!)
  mountedApps.push(app)
  return document.body
}


afterEach(() => {
  mountedApps.splice(0).forEach((app) => app.unmount())
  document.body.innerHTML = ''
  setLocale('zh-CN')
})

describe('English first-run surfaces', () => {
  it('localizes the empty conversation list and its accessible text', () => {
    setLocale('en-US')
    const root = mount(SessionList, {
      sessions: [],
      total: 0,
      loading: false,
      filtered: false,
      query: '',
    })

    expect(root.textContent).toContain('No conversations yet')
    expect(root.textContent).toContain('Sync the userscript or import a DeepSeek ZIP')
    expect(root.textContent).toContain('Updated')
    expect(root.textContent).not.toContain('还没有对话记录')
  })

  it('localizes export dialog copy and its radiogroup label', () => {
    setLocale('en-US')
    const root = mount(ExportDialog, {
      visible: true,
      selectedCount: 2,
      busy: false,
      imageDisabled: false,
      imageDisabledReason: '',
      format: 'png',
      includeThinking: false,
    })

    expect(root.textContent).toContain('Export conversation')
    expect(root.textContent).toContain('2 Q&A groups selected')
    expect(root.querySelector('[role="radiogroup"]')?.getAttribute('aria-label')).toBe('Export format')
    expect(root.textContent).not.toContain('导出聊天记录')
  })
})
