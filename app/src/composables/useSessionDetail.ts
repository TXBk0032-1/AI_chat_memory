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
    if (!shouldOpen(id)) return null
    cancelPendingWork()
    const requestGeneration = generation
    const readingPosition = loadReadingPosition(id)
    openingId.value = id
    openFailed.value = false
    loading.value = true
    backgroundLoadFailed.value = false
    error.value = ''
    try {
      const opened = await api.openSession(id, readingPosition?.seq ?? null)
      if (requestGeneration !== generation) return null
      selected.value = opened
      messageSlots.value = mergeMessageBatch(Array.from({ length: opened.message_count }), opened.messages)
      return { readingPosition, generation: requestGeneration }
    } catch (reason) {
      if (requestGeneration === generation) {
        openFailed.value = true
        error.value = String(reason)
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
    const request = (async () => {
      const messages = await api.getSessionMessages(sessionId, normalizedStart, messageBatchSize)
      if (requestGeneration !== generation || selected.value?.id !== sessionId) return false
      messageSlots.value = mergeMessageBatch(messageSlots.value, messages)
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
      if (startSeq === null) return
      try {
        await fetchBatch(startSeq, requestGeneration)
        scheduleBackgroundLoad(requestGeneration)
      } catch (reason) {
        if (requestGeneration === generation) {
          backgroundLoadFailed.value = true
          error.value = t('errors.backgroundLoad', { reason: String(reason) })
        }
      }
    }, 16)
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
