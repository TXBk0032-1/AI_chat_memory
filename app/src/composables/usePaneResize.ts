import { ref } from 'vue'

export function usePaneResize() {
  const sessionPaneWidth = ref(520)
  const resizingPanes = ref(false)
  let resizeStartX = 0
  let resizeStartWidth = 0

  function startPaneResize(event: PointerEvent) {
    resizingPanes.value = true
    resizeStartX = event.clientX
    resizeStartWidth = sessionPaneWidth.value
    ;(event.currentTarget as HTMLElement).setPointerCapture(event.pointerId)
  }

  function resizePanes(event: PointerEvent) {
    if (!resizingPanes.value) return
    const workspaceWidth = document.querySelector<HTMLElement>('.content-grid')?.clientWidth ?? 1000
    sessionPaneWidth.value = Math.min(Math.max(resizeStartWidth + event.clientX - resizeStartX, 340), workspaceWidth - 380)
  }

  function stopPaneResize() {
    resizingPanes.value = false
  }

  return { sessionPaneWidth, resizingPanes, startPaneResize, resizePanes, stopPaneResize }
}
