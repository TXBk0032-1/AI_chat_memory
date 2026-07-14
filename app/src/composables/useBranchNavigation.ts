import { ref, type Ref } from 'vue'
import { desktopApi, type DesktopApi } from '../desktop-api'
import type { BranchNode, BranchOverview, SessionOpen } from '../conversation'

export function useBranchNavigation(selected: Ref<SessionOpen | null>, api: DesktopApi = desktopApi) {
  const overview = ref<BranchOverview | null>(null)
  const loading = ref(false)
  const error = ref('')
  const activeNode = ref('')
  const mode = ref<'conversation' | 'branches'>('conversation')
  let generation = 0

  function reset() {
    generation += 1
    overview.value = null
    loading.value = false
    error.value = ''
    activeNode.value = ''
    mode.value = 'conversation'
  }

  function setOverview(value: BranchOverview | null) {
    overview.value = value
    activeNode.value = value?.default_leaf_node_id ?? ''
  }

  async function load() {
    if (!selected.value || overview.value || loading.value) return
    const requestGeneration = ++generation
    loading.value = true
    error.value = ''
    try {
      const result = await api.getSessionBranches(selected.value.id)
      if (requestGeneration !== generation) return
      overview.value = result
      activeNode.value ||= result.default_leaf_node_id
    } catch (reason) {
      if (requestGeneration === generation) error.value = String(reason)
    } finally {
      if (requestGeneration === generation) loading.value = false
    }
  }

  function show() {
    mode.value = 'branches'
    void load()
  }

  async function select(branch: BranchNode, ensureLoaded: (seq: number) => Promise<void>, locate: (seq: number) => Promise<void>) {
    activeNode.value = branch.node_id
    mode.value = 'conversation'
    await ensureLoaded(branch.seq)
    await locate(branch.seq)
  }

  return { overview, loading, error, activeNode, mode, reset, setOverview, load, show, select }
}
