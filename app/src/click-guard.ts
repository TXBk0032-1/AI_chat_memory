// Document-level capture guard against accidental duplicate activation of
// actionable controls. The swallow window is deliberately short: it absorbs
// mechanical jitter from a single interaction, while quick repeat clicks such
// as double clicks or rapid paging stay legitimate and are never swallowed
// (FE-17). Per-click logging was dropped; only an actual swallow is reported.
export const controlClickGuardDebounceMs = 150

// Controls inside these containers keep their native click behavior:
// navigation items, tabs, list rows, and rapid-paging controls.
export const clickGuardExemptSelector = '.session-pane, .sidebar, .segmented-control, .source-picker, .nav-item, .search-navigation'

const clickGuardControlSelector = 'button, a[href], select, .switch, .close-options label, [role="button"], [role="menuitem"], [role="radio"]'

export function createControlClickGuard(
  debounceMs = controlClickGuardDebounceMs,
  now: () => number = () => performance.now(),
) {
  const lastControlClicks = new WeakMap<Element, number>()

  return function preventRapidControlClick(event: MouseEvent) {
    const target = event.target instanceof Element ? event.target : null
    const control = target?.closest(clickGuardControlSelector)
    if (!control) return
    if (control.closest(clickGuardExemptSelector)) return
    const timestamp = now()
    const previous = lastControlClicks.get(control) ?? -Infinity
    if (timestamp - previous < debounceMs) {
      console.debug(`[PERF:CLICK] Duplicate click on <${control.tagName.toLowerCase()}> intercepted`)
      event.preventDefault()
      event.stopImmediatePropagation()
      return
    }
    lastControlClicks.set(control, timestamp)
  }
}
