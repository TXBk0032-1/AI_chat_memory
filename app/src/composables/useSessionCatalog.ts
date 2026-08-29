import { computed, ref } from 'vue'
import { desktopApi, type DesktopApi, type SearchMode, type SemanticStatus } from '../desktop-api'
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
  const searchMode = ref<SearchMode>('hybrid')
  const semanticStatus = ref<SemanticStatus>('disabled')
  const filtered = computed(() => Boolean(query.value || platform.value || dateFrom.value || dateTo.value || searchMode.value !== 'hybrid'))
  let generation = 0

  async function loadSessions(reset = true) {
    const started = performance.now()
    const requestGeneration = ++generation
    loading.value = true
    error.value = ''
    // On reset, capture the full previously loaded catalog before it is
    // replaced by the freshly fetched first page. A session selected
    // on a later loaded page is still present in the cached catalog; passing
    // only the new first page as the visible set would make App.vue wrongly
    // clear that selection and kick the user back to the list. The full prior
    // set is a safe superset of "still-visible" sessions for a refresh.
    const priorVisibleIds = reset ? new Set(sessions.value.map((session) => session.id)) : null
    if (reset) {
      page.value = 0
      committedQuery.value = query.value.trim()
    }
    console.log(`%c[PERF:CATALOG] loadSessions(platform="${platform.value}", q="${committedQuery.value}") started (gen=${requestGeneration})`, 'color: #059669')
    try {
      const result = await api.searchSessions({
        q: committedQuery.value || null,
        platform: platform.value || null,
        date_from: epoch(dateFrom.value),
        date_to: epoch(dateTo.value, true),
        limit: PAGE_SIZE,
        offset: page.value * PAGE_SIZE,
        mode: searchMode.value,
      })
      if (requestGeneration !== generation) {
        console.warn(`[PERF:CATALOG] loadSessions discarded due to generation mismatch (req: ${requestGeneration}, cur: ${generation})`)
        return
      }
      sessions.value = reset ? result.sessions : [...sessions.value, ...result.sessions]
      total.value = result.total
      semanticStatus.value = result.semantic_status
      searchElapsed.value = committedQuery.value ? performance.now() - started : null
      if (reset) {
        // Merge the new first page with the previously loaded catalog so a
        // selection on a later loaded page survives the refresh.
        const visibleIds = new Set(result.sessions.map((session) => session.id))
        if (priorVisibleIds) {
          for (const id of priorVisibleIds) visibleIds.add(id)
        }
        onSelectionInvalidated(visibleIds)
      }
      console.log(`%c[PERF:CATALOG] loadSessions completed: count=${result.sessions.length}, total=${result.total}, elapsed=${(performance.now() - started).toFixed(2)}ms`, 'color: #059669')
    } catch (reason) {
      if (requestGeneration === generation) {
        error.value = String(reason)
        console.error(`[PERF:CATALOG] loadSessions failed:`, reason)
      }
    } finally {
      if (requestGeneration === generation) {
        loading.value = false
      }
    }
  }

  async function loadMore() {
    if (loading.value || sessions.value.length >= total.value) return
    page.value += 1
    await loadSessions(false)
  }

  function resetFilters() {
    query.value = ''
    platform.value = ''
    dateFrom.value = ''
    dateTo.value = ''
    searchMode.value = 'hybrid'
    showFilters.value = false
    searchElapsed.value = null
    void loadSessions()
  }

  function selectPlatform(value: string) {
    platform.value = value
    void loadSessions()
  }

  function setSearchMode(mode: SearchMode) {
    searchMode.value = mode
    void loadSessions()
  }

  return {
    sessions, loading, error, query, committedQuery, platform, dateFrom, dateTo,
    showFilters, total, searchElapsed, filtered, searchMode, semanticStatus,
    loadSessions, loadMore, resetFilters, selectPlatform, setSearchMode,
  }
}
