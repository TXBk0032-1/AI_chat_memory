/** @vitest-environment happy-dom */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createThemeRerenderScheduler } from './theme-rerender'

describe('theme rerender scheduler (FE-18)', () => {
  beforeEach(() => {
    document.body.innerHTML = '<div class="mermaid-diagram" data-rendered="true"></div>'
  })

  afterEach(() => {
    document.body.innerHTML = ''
  })

  it('replaces a pending rerender instead of stacking timers on rapid switches', () => {
    vi.useFakeTimers()
    try {
      const reset = vi.fn()
      const render = vi.fn()
      const scheduler = createThemeRerenderScheduler(reset, render)

      scheduler.schedule(true)
      vi.advanceTimersByTime(100)
      // A second theme switch before the first re-render fired must replace it.
      scheduler.schedule(true)
      vi.advanceTimersByTime(180)

      expect(reset).toHaveBeenCalledTimes(2)
      expect(render).toHaveBeenCalledTimes(1)
      expect(document.querySelector('.mermaid-diagram')?.hasAttribute('data-rendered')).toBe(false)
    } finally {
      vi.useRealTimers()
    }
  })

  it('rerenders immediately when the theme is applied without animation', () => {
    vi.useFakeTimers()
    try {
      const render = vi.fn()
      const scheduler = createThemeRerenderScheduler(vi.fn(), render)

      scheduler.schedule(false)
      vi.advanceTimersByTime(0)

      expect(render).toHaveBeenCalledTimes(1)
    } finally {
      vi.useRealTimers()
    }
  })

  it('clears the pending rerender on dispose', () => {
    vi.useFakeTimers()
    try {
      const render = vi.fn()
      const scheduler = createThemeRerenderScheduler(vi.fn(), render)

      scheduler.schedule(true)
      scheduler.dispose()
      vi.advanceTimersByTime(360)

      expect(render).not.toHaveBeenCalled()
    } finally {
      vi.useRealTimers()
    }
  })
})
