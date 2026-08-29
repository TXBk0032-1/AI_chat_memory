// Schedules the Mermaid re-render that follows a theme switch. Rapid theme
// previews must replace the pending re-render instead of stacking timers,
// which caused flickering diagrams and premature transition cleanups.
export type ThemeRerenderScheduler = {
  schedule(animate: boolean): void
  dispose(): void
}

export function createThemeRerenderScheduler(reset: () => void, render: () => Promise<void> | void): ThemeRerenderScheduler {
  let timer: number | undefined

  function schedule(animate: boolean) {
    reset()
    if (timer !== undefined) window.clearTimeout(timer)
    timer = window.setTimeout(() => {
      timer = undefined
      document.querySelectorAll<HTMLElement>('.mermaid-diagram').forEach((element) => element.removeAttribute('data-rendered'))
      void render()
    }, animate ? 180 : 0)
  }

  function dispose() {
    if (timer !== undefined) {
      window.clearTimeout(timer)
      timer = undefined
    }
  }

  return { schedule, dispose }
}
