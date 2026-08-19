import { computed, ref } from 'vue'
import { desktopApi, type DesktopApi } from '../desktop-api'
import {
  loadReadingPosition,
  mergeMessageBatch,
  type Message,
  type ReadingPosition,
  type SessionOpen,
} from '../conversation'
import { translate as t } from '../i18n'

const messageBatchSize = 50

export function useSessionDetail(api: DesktopApi = desktopApi) {
  const selected = ref<SessionOpen | null>(null)
  const messageSlots = ref<Array<Message | undefined>>([])
  const loading = ref(false)
  const backgroundLoadFailed = ref(false)
  const error = ref('')
  const openingId = ref<string | null>(null)
  const openFailed = ref(false)
  const pendingBatches = new Map<string, Promise<boolean>>()
  let generation = 0
  let backgroundTimer: ReturnType<typeof setTimeout> | undefined

  const loadedMessageCount = computed(() => messageSlots.value.reduce(
    (count, message) => count + (message ? 1 : 0),
    0,
  ))

  function cancelPendingWork() {
    generation += 1
    clearTimeout(backgroundTimer)
  }

  function clear() {
    cancelPendingWork()
    openingId.value = null
    openFailed.value = false
    selected.value = null
    messageSlots.value = []
    loading.value = false
    backgroundLoadFailed.value = false
    error.value = ''
  }

  function shouldOpen(id: string) {
    if (openingId.value === id) return false
    if (openingId.value !== null) return true
    return selected.value?.id !== id || openFailed.value
  }

  async function open(id: string): Promise<{ readingPosition: ReadingPosition | null; generation: number } | null> {
    if (!shouldOpen(id)) {
      console.debug(`[PERF:DETAIL] shouldOpen(${id}) rejected: openingId=${openingId.value}, selectedId=${selected.value?.id}`)
      return null
    }
    cancelPendingWork()
    const requestGeneration = generation
    const readingPosition = loadReadingPosition(id)
    openingId.value = id
    openFailed.value = false
    loading.value = true
    backgroundLoadFailed.value = false
    error.value = ''
    const t0 = performance.now()
    console.log(`%c[PERF:DETAIL] open(${id}) started (generation=${requestGeneration})`, 'color: #0284c7; font-weight: bold')
    try {
      const opened = await api.openSession(id, readingPosition?.seq ?? null)
      const tIpc = performance.now()
      if (requestGeneration !== generation) {
        console.warn(`[PERF:DETAIL] open(${id}) discarded due to generation mismatch (req: ${requestGeneration}, cur: ${generation}) in ${(tIpc - t0).toFixed(2)}ms`)
        return null
      }
      selected.value = opened
      const tMergeStart = performance.now()
      messageSlots.value = mergeMessageBatch(Array.from({ length: opened.message_count }), opened.messages)
      const tMergeEnd = performance.now()
      console.log(
        `%c[PERF:DETAIL] open(${id}) completed: IPC=${(tIpc - t0).toFixed(2)}ms, merge=${(tMergeEnd - tMergeStart).toFixed(2)}ms, initialMsgs=${opened.messages.length}/${opened.message_count}`,
        'color: #0284c7',
      )
      return { readingPosition, generation: requestGeneration }
    } catch (reason) {
      if (requestGeneration === generation) {
        openFailed.value = true
        error.value = String(reason)
        console.error(`[PERF:DETAIL] open(${id}) failed after ${(performance.now() - t0).toFixed(2)}ms:`, reason)
      }
      return null
    } finally {
      if (requestGeneration === generation) {
        openingId.value = null
        loading.value = false
      }
    }
  }

  async function fetchBatch(startSeq: number, requestGeneration = generation) {
    if (!selected.value || requestGeneration !== generation) return false
    const normalizedStart = Math.max(0, Math.floor(startSeq / messageBatchSize) * messageBatchSize)
    const sessionId = selected.value.id
    const batchKey = `${sessionId}:${normalizedStart}`
    const pending = pendingBatches.get(batchKey)
    if (pending) return pending
    const t0 = performance.now()
    const request = (async () => {
      const messages = await api.getSessionMessages(sessionId, normalizedStart, messageBatchSize)
      const tIpc = performance.now()
      if (requestGeneration !== generation || selected.value?.id !== sessionId) return false
      messageSlots.value = mergeMessageBatch(messageSlots.value, messages)
      const tEnd = performance.now()
      console.debug(
        `[PERF:DETAIL:BATCH] fetchBatch(${sessionId}, seq=${normalizedStart}) done: IPC=${(tIpc - t0).toFixed(2)}ms, total=${(tEnd - t0).toFixed(2)}ms, count=${messages.length}`,
      )
      return messages.length > 0
    })().finally(() => pendingBatches.delete(batchKey))
    pendingBatches.set(batchKey, request)
    return request
  }

  function nextMissingBatch() {
    const index = messageSlots.value.findIndex((message) => !message)
    return index < 0 ? null : Math.floor(index / messageBatchSize) * messageBatchSize
  }

  function scheduleBackgroundLoad(requestGeneration = generation) {
    clearTimeout(backgroundTimer)
    backgroundTimer = setTimeout(async () => {
      if (requestGeneration !== generation) return
      const startSeq = nextMissingBatch()
      if (startSeq === null) {
        console.debug(`[PERF:DETAIL:BG] all background messages loaded for ${selected.value?.id}`)
        return
      }
      try {
        await fetchBatch(startSeq, requestGeneration)
        scheduleBackgroundLoad(requestGeneration)
      } catch (reason) {
        if (requestGeneration === generation) {
          backgroundLoadFailed.value = true
          error.value = t('errors.backgroundLoad', { reason: String(reason) })
          console.error(`[PERF:DETAIL:BG] background load failed at seq ${startSeq}:`, reason)
        }
      }
    }, 120)
  }

  function retryBackgroundLoad() {
    backgroundLoadFailed.value = false
    error.value = ''
    scheduleBackgroundLoad()
  }

  async function ensureMessageLoaded(seq: number) {
    if (messageSlots.value[seq]) return
    await fetchBatch(seq)
  }

  async function ensureMessagesLoaded(seqs: number[]) {
    const starts = [...new Set(seqs
      .filter((seq) => !messageSlots.value[seq])
      .map((seq) => Math.max(0, Math.floor(seq / messageBatchSize) * messageBatchSize)))]
    for (const start of starts) await fetchBatch(start)
    const missing = seqs.filter((seq) => !messageSlots.value[seq])
    if (missing.length) throw new Error(t('errors.missingExportMessages', { count: missing.length }))
  }

  function isCurrent(requestGeneration: number) {
    return requestGeneration === generation
  }

  return {
    selected,
    messageSlots,
    loading,
    backgroundLoadFailed,
    error,
    loadedMessageCount,
    open,
    shouldOpen,
    clear,
    scheduleBackgroundLoad,
    retryBackgroundLoad,
    ensureMessageLoaded,
    ensureMessagesLoaded,
    isCurrent,
    dispose: cancelPendingWork,
  }
}
