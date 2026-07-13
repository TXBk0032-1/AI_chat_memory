const SIDEBAR_COLLAPSED_KEY = 'ai-chat-memory.sidebar-collapsed'

export function loadSidebarCollapsed(): boolean {
  try {
    const saved = localStorage.getItem(SIDEBAR_COLLAPSED_KEY)
    return saved === null ? false : JSON.parse(saved) === true
  } catch {
    return false
  }
}

export function saveSidebarCollapsed(collapsed: boolean): void {
  localStorage.setItem(SIDEBAR_COLLAPSED_KEY, JSON.stringify(collapsed))
}
