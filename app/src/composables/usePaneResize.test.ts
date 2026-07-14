import { describe, expect, it } from 'vitest'
import { usePaneResize } from './usePaneResize'

describe('usePaneResize', () => {
  it('keeps pane width stable until a resize starts', () => {
    const panes = usePaneResize()
    panes.resizePanes({ clientX: 800 } as PointerEvent)
    expect(panes.sessionPaneWidth.value).toBe(520)
    expect(panes.resizingPanes.value).toBe(false)
  })
})
