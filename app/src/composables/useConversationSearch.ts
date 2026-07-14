import { ref, type Ref } from 'vue'
import { desktopApi, type DesktopApi } from '../desktop-api'
import type { SearchHit, SessionOpen } from '../conversation'

export function useConversationSearch(
  selected: Ref<SessionOpen | null>,
  query: Ref<string>,
  api: DesktopApi = desktopApi,
) {
  const hits = ref<SearchHit[]>([])
  const index = ref(-1)
  const loop = ref(false)
  let generation = 0

  function reset() {
    generation += 1
    hits.value = []
    index.value = -1
  }

  async function load() {
    const requestGeneration = ++generation
    const sessionId = selected.value?.id
    const searchQuery = query.value
    if (!sessionId || !searchQuery) {
      hits.value = []
      return
    }
    const result = await api.searchSessionHits(sessionId, searchQuery)
    if (requestGeneration === generation && selected.value?.id === sessionId && query.value === searchQuery) {
      hits.value = result
    }
  }

  return { hits, index, loop, reset, load }
}
