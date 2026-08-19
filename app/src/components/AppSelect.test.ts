/** @vitest-environment happy-dom */

import { createApp, defineComponent, h, nextTick, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import AppSelect, { type SelectOption } from './AppSelect.vue'

const styleSource = readFileSync(resolve(process.cwd(), 'src/style.css'), 'utf8')

function requiredElement<T extends Element>(root: ParentNode, selector: string) {
  const element = root.querySelector<T>(selector)
  if (!element) throw new Error(`Missing element ${selector}`)
  return element
}

function mountSelect(options: SelectOption<string>[], initialValue = 'option-1', props: Record<string, any> = {}) {
  document.body.innerHTML = '<div id="app"></div>'
  const modelValue = ref(initialValue)
  const changeSpy = vi.fn()

  const Root = defineComponent({
    setup: () => () =>
      h(AppSelect, {
        modelValue: modelValue.value,
        options,
        teleport: false,
        'onUpdate:modelValue': (val: string) => {
          modelValue.value = val
        },
        onChange: changeSpy,
        ...props,
      }),
  })

  const app = createApp(Root)
  app.mount(requiredElement(document, '#app'))

  return {
    root: document.body,
    modelValue,
    changeSpy,
    unmount: () => app.unmount(),
  }
}

describe('AppSelect component', () => {
  const sampleOptions: SelectOption<string>[] = [
    { value: 'hybrid', label: '混合搜索' },
    { value: 'semantic', label: '语义搜索' },
    { value: 'keyword', label: '关键词搜索' },
    { value: 'disabled-opt', label: '不可选项', disabled: true },
  ]

  afterEach(() => {
    document.body.innerHTML = ''
  })

  it('renders initial selected label correctly', () => {
    const { root } = mountSelect(sampleOptions, 'semantic')
    const trigger = requiredElement<HTMLButtonElement>(root, '.app-select__trigger')
    expect(trigger.textContent).toContain('语义搜索')
    expect(trigger.getAttribute('aria-expanded')).toBe('false')
  })

  it('opens and closes the dropdown menu upon trigger click', async () => {
    const { root } = mountSelect(sampleOptions, 'hybrid')
    const trigger = requiredElement<HTMLButtonElement>(root, '.app-select__trigger')

    expect(trigger.getAttribute('aria-expanded')).toBe('false')

    // Click to open
    trigger.click()
    await nextTick()

    const menu = root.querySelector('.app-select__menu')
    expect(menu).not.toBeNull()
    expect(trigger.getAttribute('aria-expanded')).toBe('true')

    // Click again to close
    trigger.click()
    await nextTick()

    expect(trigger.getAttribute('aria-expanded')).toBe('false')
  })

  it('selects option on click and updates modelValue and emits change', async () => {
    const { root, modelValue, changeSpy } = mountSelect(sampleOptions, 'hybrid')
    const trigger = requiredElement<HTMLButtonElement>(root, '.app-select__trigger')

    trigger.click()
    await nextTick()

    const optionButtons = root.querySelectorAll<HTMLButtonElement>('.app-select__option')
    expect(optionButtons.length).toBe(4)

    // Click '关键词搜索' (index 2)
    optionButtons[2].click()
    await nextTick()

    expect(modelValue.value).toBe('keyword')
    expect(changeSpy).toHaveBeenCalledWith('keyword')
    expect(trigger.getAttribute('aria-expanded')).toBe('false')
    expect(trigger.textContent).toContain('关键词搜索')
  })

  it('does not select disabled option on click', async () => {
    const { root, modelValue, changeSpy } = mountSelect(sampleOptions, 'hybrid')
    const trigger = requiredElement<HTMLButtonElement>(root, '.app-select__trigger')

    trigger.click()
    await nextTick()

    const optionButtons = root.querySelectorAll<HTMLButtonElement>('.app-select__option')
    // Click disabled option (index 3)
    optionButtons[3].click()
    await nextTick()

    expect(modelValue.value).toBe('hybrid')
    expect(changeSpy).not.toHaveBeenCalled()
    expect(trigger.getAttribute('aria-expanded')).toBe('true')
  })

  it('handles keyboard navigation: ArrowDown, ArrowUp, Enter, and Escape', async () => {
    const { root, modelValue } = mountSelect(sampleOptions, 'hybrid')
    const trigger = requiredElement<HTMLButtonElement>(root, '.app-select__trigger')

    // Press ArrowDown to open
    trigger.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }))
    await nextTick()
    expect(trigger.getAttribute('aria-expanded')).toBe('true')

    // Press ArrowDown to navigate to index 1 (semantic)
    trigger.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }))
    await nextTick()

    // Press Enter to select
    trigger.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
    await nextTick()

    expect(modelValue.value).toBe('semantic')
    expect(trigger.getAttribute('aria-expanded')).toBe('false')

    // Open again and press Escape to close
    trigger.click()
    await nextTick()
    expect(trigger.getAttribute('aria-expanded')).toBe('true')

    trigger.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    await nextTick()
    expect(trigger.getAttribute('aria-expanded')).toBe('false')
  })

  it('closes dropdown when pointerdown occurs outside', async () => {
    const { root } = mountSelect(sampleOptions, 'hybrid')
    const trigger = requiredElement<HTMLButtonElement>(root, '.app-select__trigger')

    trigger.click()
    await nextTick()
    expect(trigger.getAttribute('aria-expanded')).toBe('true')

    // Outside element click
    const outsideEl = document.createElement('div')
    document.body.appendChild(outsideEl)
    outsideEl.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true }))
    await nextTick()

    expect(trigger.getAttribute('aria-expanded')).toBe('false')
  })

  it('does not open when disabled', async () => {
    const { root } = mountSelect(sampleOptions, 'hybrid', { disabled: true })
    const trigger = requiredElement<HTMLButtonElement>(root, '.app-select__trigger')

    trigger.click()
    await nextTick()

    expect(trigger.getAttribute('aria-expanded')).toBe('false')
  })
})

describe('AppSelect styling and motion CSS assertions', () => {
  it('defines signature slide-down and fade motion classes with bezier curve', () => {
    expect(styleSource).toContain('.app-select-dropdown-enter-active')
    expect(styleSource).toContain('.app-select-dropdown-leave-active')
    expect(styleSource).toContain('.app-select-dropdown-enter-from')
    expect(styleSource).toContain('.app-select-dropdown-leave-to')

    expect(styleSource).toMatch(/\.app-select-dropdown-enter-from\s*\{[^}]*translateY\(-8px\)/)
    expect(styleSource).toMatch(/\.app-select-dropdown-enter-from\s*\{[^}]*scale\(0?\.96\)/)
    expect(styleSource).toMatch(/cubic-bezier\(\.2,\s*\.9,\s*\.25,\s*1\)/)
  })

  it('defines frosted glass and backdrop-filter for floating menu', () => {
    expect(styleSource).toMatch(/\.app-select__menu\s*\{[^}]*backdrop-filter:\s*blur\(26px\)/)
    expect(styleSource).toMatch(/\.app-select__menu\s*\{[^}]*border-radius:\s*14px/)
  })

  it('defines chevron smooth rotation', () => {
    expect(styleSource).toMatch(/\.app-select--open\s+\.app-select__chevron\s*\{[^}]*rotate\(180deg\)/)
  })

  it('defines dark mode overrides for trigger and options', () => {
    expect(styleSource).toContain('html[data-theme="dark"] .app-select__trigger')
    expect(styleSource).toContain('html[data-theme="dark"] .app-select__menu')
    expect(styleSource).toContain('html[data-theme="dark"] .app-select__option.is-selected')
  })
})
