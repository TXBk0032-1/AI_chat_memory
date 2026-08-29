/** @vitest-environment happy-dom */

import { afterEach, describe, expect, it, vi } from 'vitest'
import { clickGuardExemptSelector, controlClickGuardDebounceMs, createControlClickGuard } from './click-guard'

function clickEvent(target: Element): MouseEvent {
  const event = new MouseEvent('click', { bubbles: true, cancelable: true })
  Object.defineProperty(event, 'target', { value: target })
  return event
}

function buttonHtml(classes = ''): string {
  document.body.innerHTML = `<div><button id="plain" class="${classes}" type="button">Action</button></div>
<div class="search-navigation"><button id="nav-search" type="button">Next</button></div>
<div class="sidebar"><button id="nav-sidebar" type="button">Source</button></div>
<span id="text">plain text</span>`
  return document.body.innerHTML
}

describe('control click guard', () => {
  afterEach(() => {
    document.body.innerHTML = ''
    vi.restoreAllMocks()
  })

  it('keeps a deterministic default window and exemption list', () => {
    expect(controlClickGuardDebounceMs).toBe(150)
    expect(clickGuardExemptSelector).toContain('.search-navigation')
    expect(clickGuardExemptSelector).toContain('.sidebar')
  })

  it('swallows a duplicate click on the same control inside the window', () => {
    buttonHtml()
    let clock = 0
    const guard = createControlClickGuard(150, () => clock)
    const debugSpy = vi.spyOn(console, 'debug').mockImplementation(() => {})
    const button = requiredButton('plain')

    const first = clickEvent(button)
    guard(first)
    expect(first.defaultPrevented).toBe(false)

    clock = 100
    const duplicate = clickEvent(button)
    guard(duplicate)
    expect(duplicate.defaultPrevented).toBe(true)
    expect(debugSpy).toHaveBeenCalledTimes(1)
  })

  it('lets a legitimate second click through after the window has passed', () => {
    buttonHtml()
    let clock = 0
    const guard = createControlClickGuard(150, () => clock)
    const debugSpy = vi.spyOn(console, 'debug').mockImplementation(() => {})
    const button = requiredButton('plain')

    guard(clickEvent(button))
    clock = 200
    const second = clickEvent(button)
    guard(second)
    expect(second.defaultPrevented).toBe(false)
    expect(debugSpy).not.toHaveBeenCalled()
  })

  it('never swallows rapid clicks on exempt rapid-paging and navigation controls', () => {
    buttonHtml()
    let clock = 0
    const guard = createControlClickGuard(150, () => clock)
    for (const id of ['nav-search', 'nav-sidebar']) {
      clock = 0
      const first = clickEvent(requiredButton(id))
      guard(first)
      clock = 50
      const second = clickEvent(requiredButton(id))
      guard(second)
      expect(second.defaultPrevented).toBe(false)
    }
  })

  it('ignores clicks outside actionable controls and does not log them', () => {
    buttonHtml()
    let clock = 0
    const guard = createControlClickGuard(150, () => clock)
    const logSpy = vi.spyOn(console, 'log').mockImplementation(() => {})
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const debugSpy = vi.spyOn(console, 'debug').mockImplementation(() => {})
    const text = document.querySelector('#text') as HTMLElement

    guard(clickEvent(text))
    expect(logSpy).not.toHaveBeenCalled()
    expect(warnSpy).not.toHaveBeenCalled()
    expect(debugSpy).not.toHaveBeenCalled()
  })
})

function requiredButton(id: string): HTMLButtonElement {
  const button = document.querySelector<HTMLButtonElement>(`#${id}`)
  if (!button) throw new Error(`Missing button #${id}`)
  return button
}
