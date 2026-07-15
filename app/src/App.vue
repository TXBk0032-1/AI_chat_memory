<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { open, save } from '@tauri-apps/plugin-dialog'
import { openUrl } from '@tauri-apps/plugin-opener'
import { toJpeg, toPng } from 'html-to-image'
import {
  ArrowDown,
  ArrowUp,
  CalendarDays,
  CheckCircle2,
  ChevronDown,
  Clipboard,
  Copy,
  Download,
  FileArchive,
  GitBranch,
  LoaderCircle,
  MessageSquareText,
  MoreHorizontal,
  RefreshCw,
  Search,
  Server,
  Trash2,
  X,
} from 'lucide-vue-next'
import { useVirtualizer } from '@tanstack/vue-virtual'
import 'katex/dist/katex.min.css'
import MessageBlock from './MessageBlock.vue'
import BranchOverviewView from './BranchOverview.vue'
import AppSidebar from './components/AppSidebar.vue'
import AppTitleBar from './components/AppTitleBar.vue'
import SessionList from './components/SessionList.vue'
import SettingsDialog from './components/SettingsDialog.vue'
import SessionDialogs from './components/SessionDialogs.vue'
import ExportDialog from './components/ExportDialog.vue'
import ExportDocument from './components/ExportDocument.vue'
import {
  expandSearchHits,
  saveReadingPosition,
  type BranchNode,
  type BranchOverview,
  type SearchMatch,
  type Message,
} from './conversation'
import {
  exportDate,
  exportImagePixelRatio,
  groupConversationTurns,
  isImageExportTooLarge,
  sanitizeExportFilename,
  selectedTurnSeqs,
  serializeJson,
  serializeMarkdown,
  toExportMessages,
  type ConversationExport,
  type ConversationTurn,
  type ExportFormat,
} from './conversation-export'
import { branchMessageSeqs, branchReadingIndex, filterBranchMatches } from './branch-overview'
import { loadSidebarCollapsed, saveSidebarCollapsed } from './sidebar'
import { desktopApi, type ApiStatus, type SettingsModel } from './desktop-api'
import { useSessionCatalog } from './composables/useSessionCatalog'
import { usePaneResize } from './composables/usePaneResize'
import { useTheme } from './composables/useTheme'
import { useSettings } from './composables/useSettings'
import { useToastQueue } from './composables/useToastQueue'
import { useBranchNavigation } from './composables/useBranchNavigation'
import { useConversationSearch } from './composables/useConversationSearch'
import { useSessionDetail } from './composables/useSessionDetail'
import './style.css'

let mermaidInstance: typeof import('mermaid')['default'] | null = null
async function loadMermaid() {
  if (!mermaidInstance) {
    mermaidInstance = (await import('mermaid')).default
    mermaidInstance.initialize({ startOnLoad: false, securityLevel: 'strict', theme: effectiveTheme() === 'dark' ? 'dark' : 'neutral', fontFamily: 'Inter, Segoe UI, Microsoft YaHei, sans-serif' })
  }
  return mermaidInstance
}

function normalizeMermaidSource(source: string) {
  return source.replace(/[“”]/g, '"')
}
const messageListRef = ref<HTMLElement | null>(null)
const showClosePrompt = ref(false)
const showDeletePrompt = ref(false)
const showDetailMenu = ref(false)
const showSessionInfo = ref(false)
const exportSelecting = ref(false)
const exportSelectionLoading = ref(false)
const showExportDialog = ref(false)
const exportBusy = ref(false)
const exportImageChecking = ref(false)
const exportImageTooLong = ref(false)
const exportImageDisabledReason = ref('')
const exportFormat = ref<ExportFormat>('png')
const exportIncludeThinking = ref(false)
const exportTurns = ref<ConversationTurn[]>([])
const selectedExportTurnIds = ref(new Set<string>())
const exportLockedSessionId = ref('')
const exportLockedBranchId = ref('')
const exportRenderMessages = ref<Message[]>([])
const exportRenderModel = ref<ConversationExport | null>(null)
const exportDocumentRef = ref<InstanceType<typeof ExportDocument> | null>(null)
const pendingCloseBehavior = ref<'hide_to_tray' | 'exit' | null>(null)
const expandedThinking = ref(new Set<string>())
const settings = ref<SettingsModel>({ setup_complete: false, secret_enabled: false, allowed_origins: [], close_behavior: 'ask', tray_click_behavior: 'show_menu', theme: 'system' })
const apiStatus = ref<ApiStatus>({ service: { state: 'starting' }, userscript_connected: false })
const contextMenu = ref({ visible: false, x: 0, y: 0, selectedText: '' })
const sidebarCollapsed = ref(loadSidebarCollapsed())
const clickDebounceMs = 250
let statusTimer: number | undefined
let unlistenCloseRequest: UnlistenFn | undefined
let mermaidRenderVersion = 0
let readingPositionTimer: number | undefined
let exportPreviewGeneration = 0
const lastControlClicks = new WeakMap<Element, number>()
const detail = useSessionDetail(desktopApi)
const {
  selected,
  messageSlots,
  backgroundLoadFailed,
  loading: detailLoading,
  loadedMessageCount,
  ensureMessageLoaded,
  ensureMessagesLoaded,
} = detail
const { sessionPaneWidth, resizingPanes, startPaneResize, resizePanes, stopPaneResize } = usePaneResize()
const theme = useTheme(settings, (animate) => {
  mermaidInstance = null
  window.setTimeout(() => {
    document.querySelectorAll<HTMLElement>('.mermaid-diagram').forEach((element) => element.removeAttribute('data-rendered'))
    void renderMermaidDiagrams()
  }, animate ? 180 : 0)
})
const { effectiveTheme, commitTheme, previewTheme } = theme
const { toasts, showToast, disposeToasts } = useToastQueue()
const branches = useBranchNavigation(selected, desktopApi)
const {
  overview: branchOverview,
  loading: branchesLoading,
  error: branchesError,
  activeNode: activeBranchNode,
  mode: detailMode,
} = branches
const displayedMessageSeqs = computed(() => branchMessageSeqs(
  branchOverview.value,
  activeBranchNode.value,
  selected.value?.message_count ?? 0,
))
const displayedSeqIndexes = computed(() => new Map(displayedMessageSeqs.value.map((seq, index) => [seq, index])))
const exportTurnBySeq = computed(() => {
  const result = new Map<number, ConversationTurn>()
  for (const turn of exportTurns.value) for (const seq of turn.seqs) result.set(seq, turn)
  return result
})
const selectedExportTurns = computed(() => exportTurns.value.filter((turn) => selectedExportTurnIds.value.has(turn.id)))
const selectedExportSeqs = computed(() => selectedTurnSeqs(exportTurns.value, selectedExportTurnIds.value))
const exportImageDisabled = computed(() => exportImageChecking.value || exportImageTooLong.value)
const virtualizerOptions = computed(() => ({
  count: displayedMessageSeqs.value.length,
  getScrollElement: () => messageListRef.value,
  estimateSize: () => 190,
  overscan: 5,
  getItemKey: (index: number) => {
    const seq = displayedMessageSeqs.value[index]
    return messageSlots.value[seq]?.id ?? `${selected.value?.id ?? 'message'}-${seq}`
  },
}))
const messageVirtualizer = useVirtualizer(virtualizerOptions)
const virtualMessages = computed(() => messageVirtualizer.value.getVirtualItems())
const virtualTotalSize = computed(() => messageVirtualizer.value.getTotalSize())

function measureVirtualElement(element: unknown) {
  if (element instanceof Element) messageVirtualizer.value.measureElement(element)
}

function cancelExportSelection() {
  exportPreviewGeneration += 1
  exportSelecting.value = false
  exportSelectionLoading.value = false
  showExportDialog.value = false
  exportBusy.value = false
  exportImageChecking.value = false
  exportImageTooLong.value = false
  exportImageDisabledReason.value = ''
  exportTurns.value = []
  selectedExportTurnIds.value = new Set()
  exportLockedSessionId.value = ''
  exportLockedBranchId.value = ''
  exportRenderMessages.value = []
  exportRenderModel.value = null
}

function clearSelectedSession() {
  cancelExportSelection()
  detail.clear()
  conversationSearch.reset()
  branches.reset()
  expandedThinking.value = new Set()
}

const {
  sessions, loading, error, query, committedQuery, platform, dateFrom, dateTo,
  showFilters, total, searchElapsed, filtered, loadSessions, loadMore, resetFilters,
  selectPlatform,
} = useSessionCatalog(desktopApi, (visibleIds) => {
  if (selected.value && !visibleIds.has(selected.value.id)) {
    persistReadingPosition()
    clearSelectedSession()
  }
})
const conversationSearch = useConversationSearch(selected, committedQuery, desktopApi)
const { hits: sessionSearchHits, index: searchHitIndex, loop: loopSearch } = conversationSearch
const loadSearchHits = conversationSearch.load
const {
  showSettings, originText, secretCopied, openSettings, closeSettings, saveSettings,
  rotateSecret, copySecret, changeDataDirectory,
} = useSettings(settings, error, {
  begin: theme.beginPreview,
  accept: theme.acceptPreview,
  cancel: theme.cancelPreview,
})

function setSidebarCollapsed(collapsed: boolean) {
  sidebarCollapsed.value = collapsed
  saveSidebarCollapsed(collapsed)
}

const hasBranches = computed(() => selected.value?.has_branches ?? false)
const selectedMatches = computed<SearchMatch[]>(() => filterBranchMatches(
  expandSearchHits(sessionSearchHits.value),
  displayedMessageSeqs.value,
))
const compactReferences = computed(() => new Map((selected.value?.references ?? []).map((reference) => [reference.cite_index, reference])))

async function enterExportSelection() {
  if (!selected.value || exportSelectionLoading.value) return
  showDetailMenu.value = false
  exportSelectionLoading.value = true
  error.value = ''
  try {
    if (selected.value.has_branches && !branchOverview.value) {
      await branches.load()
      if (!branchOverview.value) throw new Error(branchesError.value || '当前分支加载失败，请重试')
    }
    const seqs = [...displayedMessageSeqs.value]
    await ensureMessagesLoaded(seqs)
    const items = seqs.map((seq) => messageSlots.value[seq]).filter((message): message is Message => Boolean(message))
    if (items.length !== seqs.length) throw new Error('当前分支消息未完整加载')
    exportTurns.value = groupConversationTurns(items)
    selectedExportTurnIds.value = new Set(exportTurns.value.map((turn) => turn.id))
    exportLockedSessionId.value = selected.value.id
    exportLockedBranchId.value = activeBranchNode.value
    exportSelecting.value = true
    detailMode.value = 'conversation'
    await nextTick()
    messageVirtualizer.value.measure()
  } catch (reason) {
    error.value = `无法进入导出选择：${String(reason)}`
  } finally {
    exportSelectionLoading.value = false
  }
}

function toggleExportTurn(turnId: string) {
  const next = new Set(selectedExportTurnIds.value)
  if (next.has(turnId)) next.delete(turnId)
  else next.add(turnId)
  selectedExportTurnIds.value = next
}

function selectAllExportTurns() {
  selectedExportTurnIds.value = new Set(exportTurns.value.map((turn) => turn.id))
}

function clearExportTurns() {
  selectedExportTurnIds.value = new Set()
}

function openExportConfirmation() {
  if (!selectedExportTurns.value.length) return
  exportFormat.value = 'png'
  exportIncludeThinking.value = false
  showExportDialog.value = true
  void prepareExportPreview()
}

function closeExportDialog() {
  exportPreviewGeneration += 1
  showExportDialog.value = false
  exportImageChecking.value = false
  exportImageTooLong.value = false
  exportImageDisabledReason.value = ''
  exportRenderMessages.value = []
  exportRenderModel.value = null
}

function selectedMessages(): Message[] {
  return selectedExportSeqs.value
    .map((seq) => messageSlots.value[seq])
    .filter((message): message is Message => Boolean(message))
    .sort((a, b) => a.seq - b.seq)
}

function createExportModel(messages: Message[]): ConversationExport {
  if (!selected.value) throw new Error('当前对话已关闭')
  const time = exportDate(selected.value.created_at || selected.value.updated_at)
  return {
    version: 1,
    title: selected.value.title || '未命名对话',
    time,
    platform: platformName(selected.value.platform),
    branch_id: selected.value.has_branches ? exportLockedBranchId.value : null,
    exported_at: new Date().toISOString(),
    messages: toExportMessages(messages, exportIncludeThinking.value),
  }
}

function blobDataUrl(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => resolve(String(reader.result))
    reader.onerror = () => reject(reader.error)
    reader.readAsDataURL(blob)
  })
}

async function localizeExportImages(root: HTMLElement) {
  const images = [...root.querySelectorAll<HTMLImageElement>('img')]
  await Promise.all(images.map(async (image) => {
    if (!/^https?:\/\//i.test(image.src)) return
    try {
      const response = await fetch(image.src)
      if (!response.ok) throw new Error(String(response.status))
      image.src = await blobDataUrl(await response.blob())
      await image.decode()
    } catch {
      const replacement = document.createElement('span')
      replacement.className = 'export-image-fallback'
      replacement.textContent = `[图片：${image.alt || image.src}]`
      image.replaceWith(replacement)
    }
  }))
}

async function prepareExportPreview() {
  if (!selected.value || !showExportDialog.value) return
  const generation = ++exportPreviewGeneration
  exportImageChecking.value = true
  exportImageTooLong.value = false
  exportImageDisabledReason.value = '正在检查图片长度'
  try {
    const messages = selectedMessages()
    if (messages.length !== selectedExportSeqs.value.length) throw new Error('所选消息未完整加载')
    exportRenderModel.value = createExportModel(messages)
    exportRenderMessages.value = messages
    await nextTick()
    const root = exportDocumentRef.value?.getElement()
    if (!root) throw new Error('图片导出文档未就绪')
    await renderExportMermaidDiagrams(root)
    await nextTick()
    await localizeExportImages(root)
    await document.fonts?.ready
    if (generation !== exportPreviewGeneration) return
    exportImageTooLong.value = isImageExportTooLarge(root.scrollWidth, root.scrollHeight)
    exportImageDisabledReason.value = exportImageTooLong.value
      ? '所选内容过长，请减少问答组或选择 Markdown/JSON'
      : ''
    if (exportImageTooLong.value && (exportFormat.value === 'png' || exportFormat.value === 'jpeg')) {
      exportFormat.value = 'md'
    }
  } catch (reason) {
    if (generation !== exportPreviewGeneration) return
    exportImageTooLong.value = true
    exportImageDisabledReason.value = `无法预检图片长度：${String(reason)}`
    if (exportFormat.value === 'png' || exportFormat.value === 'jpeg') exportFormat.value = 'md'
  } finally {
    if (generation === exportPreviewGeneration) exportImageChecking.value = false
  }
}

async function renderExportImage(format: 'png' | 'jpeg'): Promise<string> {
  await nextTick()
  const root = exportDocumentRef.value?.getElement()
  if (!root) throw new Error('图片导出文档未就绪')
  await renderExportMermaidDiagrams(root)
  await nextTick()
  await localizeExportImages(root)
  await document.fonts?.ready
  if (isImageExportTooLarge(root.scrollWidth, root.scrollHeight)) {
    throw new Error('所选内容超过单张图片尺寸限制，请减少问答组或改用 Markdown/JSON')
  }
  const options = { backgroundColor: '#ffffff', cacheBust: true, pixelRatio: exportImagePixelRatio }
  return format === 'png'
    ? toPng(root, options)
    : toJpeg(root, { ...options, quality: 0.92 })
}

async function exportSelectedConversation() {
  if (!selected.value || exportBusy.value || !selectedExportTurns.value.length) return
  if ((exportFormat.value === 'png' || exportFormat.value === 'jpeg') && exportImageDisabled.value) return
  if (selected.value.id !== exportLockedSessionId.value || activeBranchNode.value !== exportLockedBranchId.value) {
    error.value = '当前对话分支已变化，请重新选择导出内容'
    cancelExportSelection()
    return
  }
  const format = exportFormat.value
  const date = exportDate(selected.value.created_at || selected.value.updated_at)
  const filename = sanitizeExportFilename(selected.value.title, date, format)
  const path = await save({
    defaultPath: filename,
    filters: [{ name: format === 'md' ? 'Markdown' : format.toUpperCase(), extensions: [format] }],
  })
  if (typeof path !== 'string') return
  exportBusy.value = true
  error.value = ''
  let succeeded = false
  try {
    await ensureMessagesLoaded(selectedExportSeqs.value)
    const messages = selectedMessages()
    if (messages.length !== selectedExportSeqs.value.length) throw new Error('所选消息未完整加载')
    const model = createExportModel(messages)
    if (format === 'md' || format === 'json') {
      const data = format === 'md' ? serializeMarkdown(model) : serializeJson(model)
      await desktopApi.writeExportFile(path, { encoding: 'utf8', data })
    } else {
      exportRenderModel.value = model
      exportRenderMessages.value = messages
      const dataUrl = await renderExportImage(format)
      await desktopApi.writeExportFile(path, { encoding: 'base64', data: dataUrl.slice(dataUrl.indexOf(',') + 1) })
    }
    succeeded = true
    showToast(`已导出 ${selectedExportTurns.value.length} 组问答`)
  } catch (reason) {
    error.value = `导出失败：${String(reason)}`
  } finally {
    exportBusy.value = false
    if (succeeded) cancelExportSelection()
  }
}

async function selectSession(id: string) {
  persistReadingPosition()
  cancelExportSelection()
  error.value = ''
  conversationSearch.reset()
  branches.reset()
  const result = await detail.open(id)
  if (!result || !selected.value) {
    if (detail.error.value) error.value = detail.error.value
    return
  }
  const { readingPosition, generation } = result
  const opened = selected.value
  try {
    let overview: BranchOverview | null = null
    if (opened.has_branches) {
      try {
        overview = await desktopApi.getSessionBranches(id)
      } catch (reason) {
        branchesError.value = String(reason)
      }
    }
    if (!detail.isCurrent(generation)) return
    branches.setOverview(overview)
    expandedThinking.value = new Set()
    searchHitIndex.value = -1
    await nextTick()
    const readingIndex = branchReadingIndex(displayedMessageSeqs.value, readingPosition?.seq ?? null, opened.start_seq)
    messageVirtualizer.value.scrollToIndex(readingIndex, { align: 'start' })
    await nextTick()
    if (readingPosition?.offset) messageListRef.value?.scrollBy({ top: readingPosition.offset })
    void loadSearchHits()
    detail.scheduleBackgroundLoad(generation)
  } catch (reason) {
    error.value = String(reason)
  }
}

function retryBackgroundLoad() {
  error.value = ''
  detail.retryBackgroundLoad()
}

async function selectBranch(branch: BranchNode) {
  if (exportSelecting.value) return
  searchHitIndex.value = -1
  await branches.select(branch, ensureMessageLoaded, async (seq) => {
    await nextTick()
    messageVirtualizer.value.measure()
    messageVirtualizer.value.scrollToIndex(displayedSeqIndexes.value.get(seq) ?? 0, { align: 'center', behavior: 'smooth' })
  })
}

async function navigateSearch(direction: number) {
  if (!selectedMatches.value.length) return
  let next = searchHitIndex.value + direction
  if (next < 0 || next >= selectedMatches.value.length) {
    if (!loopSearch.value) return
    next = next < 0 ? selectedMatches.value.length - 1 : 0
    showToast('已循环到当前对话的另一端', 2600)
  }
  searchHitIndex.value = next
  const match = selectedMatches.value[next]
  await ensureMessageLoaded(match.seq)
  if (match.field === 'thinking') {
    const expanded = new Set(expandedThinking.value)
    expanded.add(match.message_id)
    expandedThinking.value = expanded
  }
  const displayIndex = displayedSeqIndexes.value.get(match.seq)
  if (displayIndex === undefined) return
  messageVirtualizer.value.scrollToIndex(displayIndex, { align: 'center' })
  await nextTick()
  await nextTick()
  const block = document.querySelector<HTMLElement>(`[data-message-id="${CSS.escape(match.message_id)}"]`)
  const field = block?.querySelector<HTMLElement>(`[data-search-field="${match.field}"]`)
  const hits = field?.querySelectorAll<HTMLElement>('mark.search-hit')
  hits?.[match.occurrence]?.scrollIntoView({ behavior: 'smooth', block: 'center' })
}

const loadBranches = branches.load
const showBranches = branches.show

function toggleThinking(messageId: string) {
  const next = new Set(expandedThinking.value)
  if (next.has(messageId)) next.delete(messageId)
  else next.add(messageId)
  expandedThinking.value = next
}

async function renderMermaidDiagrams() {
  const version = ++mermaidRenderVersion
  await nextTick()
  const diagrams = [...document.querySelectorAll<HTMLElement>('.mermaid-diagram:not([data-rendered])')]
  if (!diagrams.length) return
  const mermaid = await loadMermaid()
  for (const [index, element] of diagrams.entries()) {
    if (version !== mermaidRenderVersion) return
    const source = normalizeMermaidSource(decodeURIComponent(element.dataset.mermaidSource || ''))
    if (!source) continue
    try {
      const { svg, bindFunctions } = await mermaid.render(`mermaid-${version}-${index}`, source)
      element.innerHTML = svg
      element.dataset.rendered = 'true'
      bindFunctions?.(element)
    } catch (reason) {
      element.classList.add('mermaid-error')
      element.dataset.rendered = 'error'
      element.title = String(reason)
    }
  }
}

async function renderExportMermaidDiagrams(root: HTMLElement) {
  const mermaid = (await import('mermaid')).default
  mermaid.initialize({ startOnLoad: false, securityLevel: 'strict', theme: 'neutral', fontFamily: 'Inter, Segoe UI, Microsoft YaHei, sans-serif' })
  const diagrams = [...root.querySelectorAll<HTMLElement>('.mermaid-diagram:not([data-rendered])')]
  for (const [index, element] of diagrams.entries()) {
    const source = normalizeMermaidSource(decodeURIComponent(element.dataset.mermaidSource || ''))
    if (!source) continue
    try {
      const { svg } = await mermaid.render(`export-mermaid-${Date.now()}-${index}`, source)
      element.innerHTML = svg
      element.dataset.rendered = 'true'
    } catch (reason) {
      element.classList.add('mermaid-error')
      element.dataset.rendered = 'error'
      element.title = String(reason)
    }
  }
  mermaidInstance = null
}

async function removeSession() {
  if (!selected.value) return
  try {
    await desktopApi.deleteSession(selected.value.id)
    clearSelectedSession()
    showDeletePrompt.value = false
    showDetailMenu.value = false
    await loadSessions()
  } catch (reason) {
    error.value = String(reason)
  }
}

async function importZip() {
  const path = await open({ multiple: false, filters: [{ name: 'DeepSeek 导出文件', extensions: ['zip'] }] })
  if (typeof path !== 'string') return
  loading.value = true
  error.value = ''
  try {
    await desktopApi.importDeepseekZip(path)
    await loadSessions()
  } catch (reason) {
    error.value = String(reason)
  } finally {
    loading.value = false
  }
}

async function confirmClose() {
  if (!pendingCloseBehavior.value) return
  try {
    await desktopApi.confirmCloseBehavior(pendingCloseBehavior.value)
    showClosePrompt.value = false
  } catch (reason) {
    error.value = String(reason)
  }
}

function cancelClose() {
  showClosePrompt.value = false
  pendingCloseBehavior.value = null
}

function hideContextMenu() {
  contextMenu.value.visible = false
}

function hidePopupMenus() {
  hideContextMenu()
  showDetailMenu.value = false
}

function toggleDetailMenu() {
  hideContextMenu()
  showDetailMenu.value = !showDetailMenu.value
}

function preventRapidControlClick(event: MouseEvent) {
  const target = event.target instanceof Element ? event.target : null
  const control = target?.closest('button, a[href], select, .switch, .close-options label, [role="button"], [role="menuitem"], [role="radio"]')
  if (!control) return
  const now = performance.now()
  const previous = lastControlClicks.get(control) ?? -Infinity
  if (now - previous < clickDebounceMs) {
    event.preventDefault()
    event.stopImmediatePropagation()
    return
  }
  lastControlClicks.set(control, now)
}

function handleContextMenu(event: MouseEvent) {
  event.preventDefault()
  showDetailMenu.value = false
  const target = event.target as HTMLElement
  if (!target.closest('.detail-pane')) {
    hideContextMenu()
    return
  }
  const selectedText = window.getSelection()?.toString().trim() || ''
  const menuWidth = 176
  const menuHeight = 82
  contextMenu.value = {
    visible: true,
    x: Math.min(event.clientX, window.innerWidth - menuWidth - 8),
    y: Math.min(event.clientY, window.innerHeight - menuHeight - 8),
    selectedText,
  }
}

async function copyContextSelection() {
  if (!contextMenu.value.selectedText) return
  await navigator.clipboard.writeText(contextMenu.value.selectedText)
  hideContextMenu()
}

function selectConversationContent() {
  const conversation = document.querySelector('.conversation-view')
  if (!conversation) return
  const range = document.createRange()
  range.selectNodeContents(conversation)
  const selection = window.getSelection()
  selection?.removeAllRanges()
  selection?.addRange(range)
  contextMenu.value.selectedText = selection?.toString().trim() || ''
  hideContextMenu()
}

function handleContextMenuKey(event: KeyboardEvent) {
  if (event.key !== 'Escape') return
  hidePopupMenus()
  showFilters.value = false
}

async function openMarkdownLink(event: MouseEvent) {
  const link = (event.target as HTMLElement).closest<HTMLAnchorElement>('a.reference-link')
  if (!link || !/^https?:\/\//i.test(link.href)) return
  event.preventDefault()
  await openUrl(link.href)
}

async function refreshApiStatus() {
  apiStatus.value = await desktopApi.getApiStatus()
}

function formatDate(value?: string, compact = false) {
  if (!value) return '时间未知'
  const numeric = Number(value)
  const date = Number.isFinite(numeric) ? new Date(numeric * 1000) : new Date(value)
  if (Number.isNaN(date.valueOf())) return value
  return compact
    ? new Intl.DateTimeFormat('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }).format(date)
    : new Intl.DateTimeFormat('zh-CN', { year: 'numeric', month: 'long', day: 'numeric', hour: '2-digit', minute: '2-digit' }).format(date)
}

function platformName(value: string) {
  return ({ deepseek: 'DeepSeek', doubao: '豆包', kimi: 'Kimi' } as Record<string, string>)[value] ?? value
}

function roleName(value: string) {
  return value === 'user' ? '你' : value === 'assistant' ? 'AI' : value
}

function persistReadingPosition() {
  if (!selected.value || !messageListRef.value) return
  const scrollTop = messageListRef.value.scrollTop
  const first = virtualMessages.value.find((item) => item.end >= scrollTop)
  if (!first) return
  saveReadingPosition(selected.value.id, {
    seq: displayedMessageSeqs.value[first.index] ?? first.index,
    offset: Math.max(0, scrollTop - first.start),
    updatedAt: Date.now(),
  })
}

function handleMessageScroll() {
  window.clearTimeout(readingPositionTimer)
  readingPositionTimer = window.setTimeout(persistReadingPosition, 300)
  hideContextMenu()
}

onMounted(async () => {
  theme.initialize()
  window.addEventListener('keydown', handleContextMenuKey)
  document.addEventListener('click', preventRapidControlClick, true)
  document.addEventListener('scroll', hideContextMenu, true)
  unlistenCloseRequest = await listen('close-behavior-requested', () => {
    pendingCloseBehavior.value = null
    showClosePrompt.value = true
  })
  settings.value = await desktopApi.getSettings()
  commitTheme(settings.value.theme, false)
  await refreshApiStatus()
  statusTimer = window.setInterval(refreshApiStatus, 3000)
  await loadSessions()
})
watch([messageSlots, expandedThinking, detailMode], () => { void renderMermaidDiagrams() }, { flush: 'post' })
watch(committedQuery, () => {
  searchHitIndex.value = -1
  void loadSearchHits()
})
watch(exportIncludeThinking, () => {
  if (showExportDialog.value) void prepareExportPreview()
})
watch(detail.error, (value) => {
  if (value) error.value = value
})
onBeforeUnmount(() => {
  persistReadingPosition()
  detail.dispose()
  branches.reset()
  theme.dispose()
  window.removeEventListener('keydown', handleContextMenuKey)
  document.removeEventListener('click', preventRapidControlClick, true)
  document.removeEventListener('scroll', hideContextMenu, true)
  window.clearInterval(statusTimer)
  disposeToasts()
  window.clearTimeout(readingPositionTimer)
  unlistenCloseRequest?.()
})
</script>

<template>
  <div :class="['app-frame', { 'sidebar-collapsed': sidebarCollapsed }]" @click="hidePopupMenus" @contextmenu="handleContextMenu">
    <AppTitleBar />

    <AppSidebar
      :collapsed="sidebarCollapsed"
      :total="total"
      :platform="platform"
      :api-status="apiStatus"
      @collapse="setSidebarCollapsed"
      @select-platform="selectPlatform"
      @open-settings="openSettings"
    />

    <main class="workspace">
      <header class="workspace-header">
        <div><h1>全部对话</h1><p>集中查找和查看已同步的 AI 对话</p></div>
        <div class="header-actions">
          <button class="secondary-button" :disabled="loading" @click="loadSessions()"><RefreshCw :size="16" :class="{ spinning: loading }" />刷新</button>
          <button class="primary-button" @click="importZip"><FileArchive :size="16" />导入 ZIP</button>
        </div>
      </header>

      <section class="control-bar">
        <div class="search-stack">
          <label class="search-field"><Search :size="17" /><input v-model="query" placeholder="搜索标题和消息内容" @input="searchElapsed=null" @keyup.enter="loadSessions()" /><button v-if="query" title="清除搜索" @click="query=''; searchElapsed=null; loadSessions()"><X :size="15" /></button></label>
          <Transition name="search-summary"><div v-if="searchElapsed !== null" class="search-summary">找到 {{ total }} 条结果 · {{ searchElapsed.toFixed(0) }} 毫秒</div></Transition>
        </div>
        <button :class="['filter-button', { active: showFilters || filtered, expanded: showFilters }]" :aria-expanded="showFilters" @click="showFilters=!showFilters"><CalendarDays :size="16" />日期筛选<ChevronDown class="filter-chevron" :size="14" /></button>
        <span class="result-count">{{ sessions.length }} / {{ total }}</span>
      </section>

      <Transition name="filter-panel">
        <section v-if="showFilters" class="filter-panel">
          <label><span>开始日期</span><input v-model="dateFrom" type="date" /></label>
          <label><span>结束日期</span><input v-model="dateTo" type="date" /></label>
          <button class="primary-button compact" @click="loadSessions()">应用</button>
          <button v-if="filtered" class="text-button" @click="resetFilters">清除条件</button>
        </section>
      </Transition>

      <div v-if="error || apiStatus.service.state === 'failed'" class="alert-bar">
        <Server :size="17" />
        <span>{{ error || `本地同步服务启动失败：${apiStatus.service.message || '未知错误'}` }}</span>
        <button title="关闭" @click="error=''"><X :size="15" /></button>
      </div>

      <section :class="['content-grid', { resizing: resizingPanes }]" :style="{ '--session-pane-width': `${sessionPaneWidth}px` }">
        <SessionList
          :sessions="sessions"
          :total="total"
          :loading="loading"
          :selected-id="selected?.id"
          :filtered="filtered"
          :query="committedQuery"
          @select="selectSession"
          @load-more="loadMore"
        />

        <div class="pane-resizer" role="separator" aria-label="调整对话列表和内容宽度" aria-orientation="vertical" tabindex="0" @pointerdown="startPaneResize" @pointermove="resizePanes" @pointerup="stopPaneResize" @pointercancel="stopPaneResize"></div>

        <aside class="detail-pane">
          <div v-if="detailLoading" class="loading-state"><LoaderCircle class="spinning" :size="22" /><span>正在打开对话</span></div>
          <template v-else-if="selected">
            <div class="detail-header">
              <div class="detail-title"><span class="platform-badge"><i :class="selected.platform"></i>{{ platformName(selected.platform) }}</span><h2>{{ selected.title || '未命名对话' }}</h2><p>{{ displayedMessageSeqs.length }} 条消息<span v-if="branchOverview && displayedMessageSeqs.length < selected.message_count"> · 共 {{ selected.message_count }} 个版本节点</span> · {{ formatDate(selected.updated_at) }}<span v-if="loadedMessageCount < selected.message_count" class="load-progress"> · 已加载 {{ loadedMessageCount }}/{{ selected.message_count }}</span><button v-if="backgroundLoadFailed" class="inline-retry" @click="retryBackgroundLoad">重试</button></p></div>
              <div class="detail-actions" @click.stop>
                <button class="icon-button" title="更多操作" aria-haspopup="menu" :aria-expanded="showDetailMenu" @click="toggleDetailMenu"><MoreHorizontal :size="19" /></button>
                <div v-if="showDetailMenu" class="detail-menu" role="menu">
                  <button role="menuitem" :disabled="exportSelectionLoading" @click="enterExportSelection"><Download :size="14" />{{ exportSelectionLoading ? '正在准备' : '导出聊天记录' }}</button>
                  <button role="menuitem" @click="showSessionInfo=true; showDetailMenu=false">对话详细信息</button>
                  <button class="danger" role="menuitem" @click="showDeletePrompt=true; showDetailMenu=false"><Trash2 :size="14" />删除对话</button>
                </div>
              </div>
            </div>
            <div v-if="hasBranches" :class="['segmented-control', { branches: detailMode === 'branches' }]">
              <span class="segmented-highlight" aria-hidden="true"></span>
              <button :class="{ active: detailMode === 'conversation' }" :disabled="exportSelecting" @click="detailMode='conversation'"><MessageSquareText :size="15" />对话</button>
              <button :class="{ active: detailMode === 'branches' }" :disabled="exportSelecting" @click="showBranches"><GitBranch :size="15" />分支预览</button>
            </div>
            <div v-if="exportSelecting" class="export-selection-toolbar">
              <strong>已选择 {{ selectedExportTurns.length }} / {{ exportTurns.length }} 组问答</strong>
              <div>
                <button class="text-button" @click="selectAllExportTurns">全选</button>
                <button class="text-button" @click="clearExportTurns">取消全选</button>
                <button class="secondary-button compact" @click="cancelExportSelection">取消</button>
                <button class="primary-button compact" :disabled="!selectedExportTurns.length" @click="openExportConfirmation"><Download :size="14" />导出所选</button>
              </div>
            </div>
            <div v-if="committedQuery && detailMode === 'conversation'" class="search-navigation">
              <span>{{ selectedMatches.length ? `${Math.max(searchHitIndex + 1, 0)} / ${selectedMatches.length}` : '当前对话无正文命中' }}</span>
              <button class="icon-button" title="上一个命中" :disabled="!selectedMatches.length" @click="navigateSearch(-1)"><ArrowUp :size="15" /></button>
              <button class="icon-button" title="下一个命中" :disabled="!selectedMatches.length" @click="navigateSearch(1)"><ArrowDown :size="15" /></button>
              <label><input v-model="loopSearch" type="checkbox" />循环</label>
            </div>
            <div ref="messageListRef" :class="['message-list', { 'branch-mode': detailMode === 'branches' }]" @scroll.passive="handleMessageScroll" @click="openMarkdownLink">
              <div v-if="selectedMatches.length" class="search-scroll-markers" aria-hidden="true">
                <i v-for="(match, index) in selectedMatches" :key="`${match.message_id}-${match.field}-${index}`" :style="{ top: `${((displayedSeqIndexes.get(match.seq) ?? 0) + 0.5) / Math.max(displayedMessageSeqs.length, 1) * 100}%` }"></i>
              </div>
              <Transition name="detail-camera" mode="out-in">
                <div v-if="detailMode === 'conversation'" key="conversation" class="conversation-view virtual-conversation" :style="{ height: `${virtualTotalSize}px` }">
                  <div
                    v-for="virtualMessage in virtualMessages"
                    :key="String(virtualMessage.key)"
                    :ref="measureVirtualElement"
                    :class="['virtual-message', { 'export-selecting': exportSelecting, 'export-turn-selected': selectedExportTurnIds.has(exportTurnBySeq.get(displayedMessageSeqs[virtualMessage.index])?.id || '') }]"
                    :data-index="virtualMessage.index"
                    :style="{ transform: `translateY(${virtualMessage.start}px)` }"
                  >
                    <label v-if="exportSelecting && exportTurnBySeq.get(displayedMessageSeqs[virtualMessage.index])?.seqs[0] === displayedMessageSeqs[virtualMessage.index]" class="export-turn-checkbox">
                      <input type="checkbox" :checked="selectedExportTurnIds.has(exportTurnBySeq.get(displayedMessageSeqs[virtualMessage.index])!.id)" :aria-label="`选择第 ${(exportTurns.indexOf(exportTurnBySeq.get(displayedMessageSeqs[virtualMessage.index])!) + 1)} 组问答`" @change="toggleExportTurn(exportTurnBySeq.get(displayedMessageSeqs[virtualMessage.index])!.id)" />
                    </label>
                    <MessageBlock
                      v-if="messageSlots[displayedMessageSeqs[virtualMessage.index]]"
                      :message="messageSlots[displayedMessageSeqs[virtualMessage.index]]!"
                      :references="compactReferences"
                      :query="committedQuery"
                      :expanded="expandedThinking.has(messageSlots[displayedMessageSeqs[virtualMessage.index]]!.id)"
                      :formatted-date="formatDate(messageSlots[displayedMessageSeqs[virtualMessage.index]]!.created_at, true)"
                      :role-label="roleName(messageSlots[displayedMessageSeqs[virtualMessage.index]]!.role)"
                      @toggle-thinking="toggleThinking"
                      @content-rendered="renderMermaidDiagrams"
                    />
                    <div v-else class="message-placeholder" @vue:mounted="ensureMessageLoaded(displayedMessageSeqs[virtualMessage.index])"><LoaderCircle class="spinning" :size="16" /><span>加载消息</span></div>
                  </div>
                </div>
                <div v-else key="branches" class="branch-view">
                  <div v-if="branchesLoading" class="branch-state"><LoaderCircle class="spinning" :size="22" /><span>正在构建分支预览</span></div>
                  <div v-else-if="branchesError" class="branch-state error"><GitBranch :size="25" /><strong>分支预览加载失败</strong><span>{{ branchesError }}</span><button class="secondary-button compact" @click="loadBranches">重试</button></div>
                  <div v-else-if="branchOverview && !branchOverview.nodes.length" class="branch-state"><GitBranch :size="28" /><strong>没有可预览的分支节点</strong></div>
                  <BranchOverviewView v-else-if="branchOverview" :overview="branchOverview" :active-node-id="activeBranchNode" @select="selectBranch" />
                </div>
              </Transition>
            </div>
          </template>
          <div v-else class="detail-placeholder"><MessageSquareText :size="34" /><strong>选择一条对话</strong><span>消息内容会显示在这里</span></div>
        </aside>
      </section>
    </main>

    <SettingsDialog
      v-model:settings="settings"
      v-model:origin-text="originText"
      :visible="showSettings"
      :secret-copied="secretCopied"
      @close="closeSettings"
      @save="saveSettings"
      @preview-theme="previewTheme"
      @change-data-directory="changeDataDirectory"
      @copy-secret="copySecret"
      @rotate-secret="rotateSecret"
    />

    <SessionDialogs
      v-model:pending-close-behavior="pendingCloseBehavior"
      :selected="selected"
      :show-info="showSessionInfo"
      :show-delete="showDeletePrompt"
      :show-close="showClosePrompt"
      :format-date="formatDate"
      :platform-name="platformName"
      @close-info="showSessionInfo=false"
      @close-delete="showDeletePrompt=false"
      @delete="removeSession"
      @cancel-close="cancelClose"
      @confirm-close="confirmClose"
    />
    <ExportDialog
      v-model:format="exportFormat"
      v-model:include-thinking="exportIncludeThinking"
      :visible="showExportDialog"
      :selected-count="selectedExportTurns.length"
      :busy="exportBusy"
      :image-disabled="exportImageDisabled"
      :image-disabled-reason="exportImageDisabledReason"
      @close="closeExportDialog"
      @export="exportSelectedConversation"
    />
    <div v-if="exportRenderModel" class="export-document-host" aria-hidden="true">
      <ExportDocument
        ref="exportDocumentRef"
        :title="exportRenderModel.title"
        :time="exportRenderModel.time"
        :platform="exportRenderModel.platform"
        :messages="exportRenderMessages"
        :references="compactReferences"
        :include-thinking="exportIncludeThinking"
      />
    </div>
    <Transition name="context-menu">
      <div v-if="contextMenu.visible" class="context-menu" role="menu" :style="{ left: `${contextMenu.x}px`, top: `${contextMenu.y}px` }" @click.stop>
        <button role="menuitem" :disabled="!contextMenu.selectedText" @click="copyContextSelection"><Copy :size="15" /><span>复制</span><kbd>Ctrl+C</kbd></button>
        <button role="menuitem" @click="selectConversationContent"><Clipboard :size="15" /><span>全选对话内容</span><kbd>Ctrl+A</kbd></button>
      </div>
    </Transition>
    <TransitionGroup name="toast" tag="div" class="toast-stack" aria-live="polite" aria-atomic="false">
      <article
        v-for="notice in toasts"
        :key="notice.id"
        class="toast-notice"
        role="status"
        :style="{ '--toast-duration': `${notice.duration}ms` }"
      >
        <span class="toast-icon" aria-hidden="true"><CheckCircle2 :size="19" /></span>
        <strong>{{ notice.message }}</strong>
        <span class="toast-progress" aria-hidden="true"><i></i></span>
      </article>
    </TransitionGroup>
  </div>
</template>
