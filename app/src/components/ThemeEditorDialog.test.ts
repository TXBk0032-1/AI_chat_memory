/** @vitest-environment happy-dom */

import { createApp, defineComponent, h, nextTick, ref } from 'vue'
import { describe, expect, it, vi, beforeEach } from 'vitest'
import ThemeEditorDialog from './ThemeEditorDialog.vue'
import type { ThemeDefinition } from '../theme/types'

function requiredElement<T extends Element>(root: ParentNode, selector: string) {
  const element = root.querySelector<T>(selector)
  if (!element) throw new Error(`Missing element ${selector}`)
  return element
}

describe('ThemeEditorDialog component', () => {
  beforeEach(() => {
    document.body.innerHTML = '<div id="app"></div>'
  })

  const sampleTheme: ThemeDefinition = {
    id: 'custom_123',
    name: 'Custom Sunset',
    nameKey: '',
    isDark: false,
    isCustom: true,
    config: {
      primary: 'rgb(245, 171, 53)',
      font: 'rgb(33, 33, 33)',
      extInfo: {
        '--color-app-background': 'rgb(253, 249, 242)',
        '--color-main-background': 'rgb(255, 255, 255)',
      },
    },
  }

  function mountDialog(props: {
    show: boolean
    themeDef: ThemeDefinition | null
    isNew?: boolean
  }) {
    const showRef = ref(props.show)
    const saveSpy = vi.fn()
    const deleteSpy = vi.fn()

    const Root = defineComponent({
      setup: () => () =>
        h(ThemeEditorDialog, {
          show: showRef.value,
          themeDef: props.themeDef,
          isNew: props.isNew,
          'onUpdate:show': (val: boolean) => {
            showRef.value = val
          },
          onSave: saveSpy,
          onDelete: deleteSpy,
        }),
    })

    const app = createApp(Root)
    app.mount(requiredElement(document, '#app'))

    return {
      root: document.body,
      showRef,
      saveSpy,
      deleteSpy,
      unmount: () => app.unmount(),
    }
  }

  it('renders dialog when show is true', async () => {
    const { root, unmount } = mountDialog({
      show: true,
      themeDef: sampleTheme,
      isNew: false,
    })

    await nextTick()
    const dialog = root.querySelector('.theme-editor-dialog')
    expect(dialog).toBeTruthy()

    const nameInput = root.querySelector<HTMLInputElement>('input#theme-name-input')
    expect(nameInput?.value).toBe('Custom Sunset')
    unmount()
  })

  it('validates name requirement before save', async () => {
    const { root, saveSpy, unmount } = mountDialog({
      show: true,
      themeDef: null,
      isNew: true,
    })

    await nextTick()
    const nameInput = requiredElement<HTMLInputElement>(root, 'input#theme-name-input')
    nameInput.value = ''
    nameInput.dispatchEvent(new Event('input'))
    await nextTick()

    const saveBtn = requiredElement<HTMLButtonElement>(root, '.btn-primary')
    saveBtn.click()
    await nextTick()

    expect(saveSpy).not.toHaveBeenCalled()
    expect(root.querySelector('.theme-error-banner')).toBeTruthy()
    unmount()
  })

  it('emits save event with updated theme definition', async () => {
    const { root, saveSpy, unmount } = mountDialog({
      show: true,
      themeDef: sampleTheme,
      isNew: false,
    })

    await nextTick()
    const nameInput = requiredElement<HTMLInputElement>(root, 'input#theme-name-input')
    nameInput.value = 'Updated Sunset'
    nameInput.dispatchEvent(new Event('input'))
    await nextTick()

    const saveBtn = requiredElement<HTMLButtonElement>(root, '.btn-primary')
    saveBtn.click()
    await nextTick()

    expect(saveSpy).toHaveBeenCalled()
    const [savedTheme, activate] = saveSpy.mock.calls[0]
    expect(savedTheme.name).toBe('Updated Sunset')
    expect(savedTheme.isCustom).toBe(true)
    expect(activate).toBe(true)
    unmount()
  })

  it('handles delete action with confirmation', async () => {
    window.confirm = vi.fn().mockReturnValue(true)

    const { root, deleteSpy, unmount } = mountDialog({
      show: true,
      themeDef: sampleTheme,
      isNew: false,
    })

    await nextTick()
    const deleteBtn = requiredElement<HTMLButtonElement>(root, '.btn-danger')
    deleteBtn.click()
    await nextTick()

    expect(deleteSpy).toHaveBeenCalledWith('custom_123')
    unmount()
  })

  it('switches between light and dark modes in editor', async () => {
    const { root, unmount } = mountDialog({
      show: true,
      themeDef: sampleTheme,
      isNew: false,
    })

    await nextTick()
    const modeButtons = root.querySelectorAll<HTMLButtonElement>('.mode-btn')
    expect(modeButtons.length).toBe(2)

    // Switch to dark
    modeButtons[1].click()
    await nextTick()

    const badge = requiredElement<HTMLElement>(root, '.mini-badge')
    expect(badge.textContent?.trim()).toBe('Dark')
    unmount()
  })

  it('preserves alpha channel of app/main background colors across load and save', async () => {
    // The background color inputs used to coerce loaded values through toHex6,
    // which truncates the alpha channel (#RRGGBBAA -> #RRGGBB). A theme that
    // ships a translucent app/main background would therefore lose its alpha
    // the moment the editor opened it, and saving would persist the opaque
    // value. Both inputs must keep the alpha when present.
    const translucentTheme: ThemeDefinition = {
      ...sampleTheme,
      config: {
        ...sampleTheme.config,
        extInfo: {
          '--color-app-background': 'rgba(253, 249, 242, 0.5)',
          '--color-main-background': 'rgba(255, 255, 255, 0.7)',
        },
      },
    }

    const { root, saveSpy, unmount } = mountDialog({
      show: true,
      themeDef: translucentTheme,
      isNew: false,
    })

    await nextTick()

    // The dialog renders several color picker rows in DOM order: primary,
    // app background, main background, font color, ... The app/main
    // background text inputs are the 2nd and 3rd .color-picker-row text inputs.
    const pickerTextInputs = root.querySelectorAll<HTMLInputElement>(
      '.color-picker-row input[type="text"]',
    )
    expect(pickerTextInputs.length).toBeGreaterThanOrEqual(3)
    const appBgInput = pickerTextInputs[1]
    const mainBgInput = pickerTextInputs[2]
    // Loaded value retains the alpha (not truncated to opaque rgb)
    expect(appBgInput.value).toMatch(/0\.5/)
    expect(mainBgInput.value).toMatch(/0\.7/)

    const saveBtn = requiredElement<HTMLButtonElement>(root, '.btn-primary')
    saveBtn.click()
    await nextTick()

    expect(saveSpy).toHaveBeenCalled()
    const [savedTheme] = saveSpy.mock.calls[0]
    const ext = savedTheme.config.extInfo
    expect(ext['--color-app-background']).toMatch(/0\.5/)
    expect(ext['--color-main-background']).toMatch(/0\.7/)
    unmount()
  })
})
