import { computed, ref } from 'vue'
import { desktopApi, type DesktopApi } from '../desktop-api'
import type { SessionSummary } from '../conversation'

const PAGE_SIZE = 100

function epoch(value: string, end = false) {
  if (!value) return null
  return String(new Date(`${value}T${end ? '23:59:59' : '00:00:00'}`).getTime() / 1000)
}

export function useSessionCatalog(
  api: DesktopApi = desktopApi,
  onSelectionInvalidated: (visibleIds: Set<string>) => void = () => {},
) {
  const sessions = ref<SessionSummary[]>([])
  const loading = ref(false)
  const error = ref('')
  const query = ref('')
  const committedQuery = ref('')
  const platform = ref('')
  const dateFrom = ref('')
  const dateTo = ref('')
  const showFilters = ref(false)
  const total = ref(0)
  const page = ref(0)
  const searchElapsed = ref<number | null>(null)
  const filtered = computed(() => Boolean(query.value || platform.value || dateFrom.value || dateTo.value))

  async function loadSessions(reset = true) {
    const started = performance.now()
    loading.value = true
    error.value = ''
    if (reset) {
      page.value = 0
      committedQuery.value = query.value.trim()
    }
    try {
      const result = await api.searchSessions({
        q: committedQuery.value || null,
        platform: platform.value || null,
        date_from: epoch(dateFrom.value),
        date_to: epoch(dateTo.value, true),
        limit: PAGE_SIZE,
        offset: page.value * PAGE_SIZE,
      })
      sessions.value = reset ? result.sessions : [...sessions.value, ...result.sessions]
      total.value = result.total
      searchElapsed.value = committedQuery.value ? performance.now() - started : null
      if (reset) onSelectionInvalidated(new Set(result.sessions.map((session) => session.id)))
    } catch (reason) {
      error.value = String(reason)
    } finally {
      loading.value = false
    }
  }

  async function loadMore() {
    page.value += 1
    await loadSessions(false)
  }

  function resetFilters() {
    query.value = ''
    platform.value = ''
    dateFrom.value = ''
    dateTo.value = ''
    showFilters.value = false
    searchElapsed.value = null
    void loadSessions()
  }

  function selectPlatform(value: string) {
    platform.value = value
    void loadSessions()
  }

  return {
    sessions, loading, error, query, committedQuery, platform, dateFrom, dateTo,
    showFilters, total, searchElapsed, filtered, loadSessions, loadMore, resetFilters,
    selectPlatform,
  }
}
