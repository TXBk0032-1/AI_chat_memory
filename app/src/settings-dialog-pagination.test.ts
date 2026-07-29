/** @vitest-environment happy-dom */

import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { createApp, defineComponent, h, nextTick, ref } from 'vue'
import { describe, expect, it, vi } from 'vitest'
import SettingsDialog from './components/SettingsDialog.vue'
import type { SettingsModel } from './desktop-api'

const styleSource = readFileSync(resolve(process.cwd(), 'src/style.css'), 'utf8')
const tauriConfig = JSON.parse(
  readFileSync(resolve(process.cwd(), 'src-tauri/tauri.conf.json'), 'utf8'),
) as { app: { windows: Array<{ minWidth: number }> } }
const minimumWindowWidth = tauriConfig.app.windows[0].minWidth

function ruleFor(selector: string) {
  const escapedSelector = selector.replace(/[.*+?^\${}()|[\]\\]/g, '\\$&')
  return styleSource.match(new RegExp(escapedSelector + '\\s*\\{([^}]*)\\}'))?.[1]
}

function settingsFixture(): SettingsModel {
  return {
    setup_complete: true,
    secret_enabled: true,
    secret: 'test-secret',
    allowed_origins: ['https://example.test'],
    data_directory: 'C:\\test-data',
    close_behavior: 'ask',
    tray_click_behavior: 'show_menu',
    theme: 'system',
    semantic_search: {
      enabled: true,
      default_mode: 'hybrid',
      backend: 'local',
      local: { model: 'test-model', device: 'auto', dtype: 'auto' },
      ollama: { base_url: 'http://127.0.0.1:11434', model: 'test-model' },
      llama_cpp: { base_url: 'http://127.0.0.1:8080/v1', model: 'test-model' },
      openai_compatible: { base_url: 'https://example.test/v1', model: 'test-model' },
    },
    mcp_enabled: true,
    cloud_sync: { enabled: false, base_url: '', root_path: '', username: '', encryption_enabled: false },
  }
}

function requiredElement<T extends Element>(root: ParentNode, selector: string): T {
  const element = root.querySelector<T>(selector)
  if (!element) throw new Error(`Missing element ${selector}`)
  return element
}

function buttonWithText(root: ParentNode, text: string): HTMLButtonElement {
  const button = [...root.querySelectorAll<HTMLButtonElement>('button')]
    .find((candidate) => candidate.textContent?.trim() === text)
  if (!button) throw new Error(`Missing button "${text}"`)
  return button
}

function selectWithLabel(root: ParentNode, text: string): HTMLSelectElement {
  const label = [...root.querySelectorAll<HTMLLabelElement>('label')]
    .find((candidate) => candidate.textContent?.includes(text))
  const select = label?.querySelector<HTMLSelectElement>('select')
  if (!select) throw new Error(`Missing select labelled "${text}"`)
  return select
}

function mountDialog() {
  document.body.innerHTML = '<div id="app"></div>'
  const visible = ref(true)
  const close = vi.fn()
  const save = vi.fn()
  const settings = settingsFixture()
  const Root = defineComponent({
    setup: () => () => h(SettingsDialog, {
      visible: visible.value,
      secretCopied: false,
      settings,
      originText: 'https://example.test',
      onClose: close,
      onSave: save,
    }),
  })
  const app = createApp(Root)
  app.mount(requiredElement(document, '#app'))

  return {
    root: document.body,
    visible,
    close,
    save,
    settings,
    unmount() {
      app.unmount()
      document.body.innerHTML = ''
    },
  }
}

describe('settings dialog pagination', () => {
  it('switches one labelled panel at a time and retains shared form actions', async () => {
    const harness = mountDialog()
    try {
      const generalButton = requiredElement<HTMLButtonElement>(harness.root, '#settings-navigation-general')
      const semanticButton = requiredElement<HTMLButtonElement>(harness.root, '#settings-navigation-semantic')
      const generalPanel = requiredElement<HTMLElement>(harness.root, '#settings-page-general')
      const semanticPanel = requiredElement<HTMLElement>(harness.root, '#settings-page-semantic')

      expect(generalButton.getAttribute('aria-current')).toBe('page')
      expect(generalButton.getAttribute('aria-controls')).toBe('settings-page-general')
      expect(generalPanel.getAttribute('aria-labelledby')).toBe('settings-navigation-general')
      expect(generalPanel.style.display).not.toBe('none')
      expect(semanticPanel.style.display).toBe('none')

      const closeBehavior = selectWithLabel(generalPanel, '关闭窗口后')
      closeBehavior.value = 'exit'
      closeBehavior.dispatchEvent(new Event('change'))
      semanticButton.click()
      await nextTick()

      expect(generalButton.hasAttribute('aria-current')).toBe(false)
      expect(semanticButton.getAttribute('aria-current')).toBe('page')
      expect(generalPanel.style.display).toBe('none')
      expect(semanticPanel.style.display).not.toBe('none')
      expect(harness.settings.close_behavior).toBe('exit')

      generalButton.click()
      await nextTick()
      expect(selectWithLabel(generalPanel, '关闭窗口后').value).toBe('exit')

      const footers = harness.root.querySelectorAll('footer')
      expect(footers).toHaveLength(1)
      buttonWithText(footers[0], '保存设置').click()
      expect(harness.save).toHaveBeenCalledOnce()
      expect(harness.close).not.toHaveBeenCalled()
    } finally {
      harness.unmount()
    }
  })

  it('returns to the general page whenever the dialog reopens', async () => {
    const harness = mountDialog()
    try {
      requiredElement<HTMLButtonElement>(harness.root, '#settings-navigation-connections').click()
      await nextTick()
      expect(
        requiredElement(harness.root, '#settings-navigation-connections').getAttribute('aria-current'),
      ).toBe('page')

      harness.visible.value = false
      await nextTick()
      harness.visible.value = true
      await nextTick()

      expect(requiredElement(harness.root, '#settings-navigation-general').getAttribute('aria-current')).toBe('page')
      expect(requiredElement<HTMLElement>(harness.root, '#settings-page-general').style.display).not.toBe('none')
      expect(requiredElement<HTMLElement>(harness.root, '#settings-page-connections').style.display).toBe('none')
      buttonWithText(harness.root, '取消').click()
      expect(harness.close).toHaveBeenCalledOnce()
      expect(harness.save).not.toHaveBeenCalled()
    } finally {
      harness.unmount()
    }
  })

  it('uses a scalable side navigation and folds it above content at the app minimum width', () => {
    const dialog = ruleFor('.settings-dialog')
    const layout = ruleFor('.settings-layout')
    const navigation = ruleFor('.settings-navigation')

    expect(dialog).toMatch(/\bwidth:\s*min\(820px,\s*94vw\)\s*;/)
    expect(dialog).toMatch(/\bheight:\s*min\(720px,\s*90vh\)\s*;/)
    expect(layout).toMatch(/\bdisplay:\s*grid\s*;/)
    expect(layout).toMatch(/\bgrid-template-columns:\s*156px\s+minmax\(0,\s*1fr\)\s*;/)
    expect(navigation).toMatch(/\bdisplay:\s*flex\s*;/)
    expect(navigation).toMatch(/\bflex-direction:\s*column\s*;/)

    const responsiveStart = styleSource.lastIndexOf(`@media (max-width: ${minimumWindowWidth}px)`)
    expect(responsiveStart).toBeGreaterThan(-1)
    const responsive = styleSource.slice(responsiveStart, responsiveStart + 1800)
    expect(responsive).toMatch(/\.settings-dialog-backdrop\s*\{[^}]*padding:\s*12px\s*;/s)
    expect(responsive).toMatch(/\.settings-layout\s*\{[^}]*grid-template-columns:\s*1fr\s*;/s)
    expect(responsive).toMatch(/\.settings-navigation\s*\{[^}]*overflow-x:\s*auto\s*;/s)
    expect(styleSource).toContain('html[data-theme="dark"] .settings-navigation')
    expect(styleSource).toContain('html[data-theme="dark"] .settings-navigation__button.active')
    expect(styleSource).toMatch(/@media \(prefers-reduced-motion: reduce\)[\s\S]*settings-navigation__button/)
  })
})
