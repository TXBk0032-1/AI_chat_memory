<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { open } from '@tauri-apps/plugin-dialog'
import { openUrl } from '@tauri-apps/plugin-opener'
import {
  Archive,
  ArrowDown,
  ArrowUp,
  CalendarDays,
  Check,
  ChevronDown,
  Clipboard,
  Copy,
  FileArchive,
  GitBranch,
  Inbox,
  LoaderCircle,
  MessageSquareText,
  MoreHorizontal,
  RefreshCw,
  Search,
  Server,
  Settings,
  ShieldCheck,
  Monitor,
  Moon,
  PanelLeftClose,
  PanelLeftOpen,
  Sun,
  Trash2,
  X,
} from 'lucide-vue-next'
import { useVirtualizer } from '@tanstack/vue-virtual'
import 'katex/dist/katex.min.css'
import MessageBlock from './MessageBlock.vue'
import BranchOverviewView from './BranchOverview.vue'
import {
  expandSearchHits,
  loadReadingPosition,
  mergeMessageBatch,
  saveReadingPosition,
  type BranchNode,
  type BranchOverview,
  type Message,
  type SearchHit,
  type SearchMatch,
  type SessionOpen,
  type SessionSummary,
} from './conversation'
import { escapeTitle } from './markdown'
import { branchConversation } from './branch-overview'
import { loadSidebarCollapsed, saveSidebarCollapsed } from './sidebar'
import './style.css'

type SettingsModel = {
  setup_complete: boolean
  secret_enabled: boolean
  secret?: string
  allowed_origins: string[]
  data_directory?: string
  close_behavior: 'ask' | 'hide_to_tray' | 'exit'
  tray_click_behavior: 'show_menu' | 'open_window' | 'no_action'
  theme: 'system' | 'light' | 'dark'
}
type ApiStatus = { service: { state: string; message?: string }; userscript_connected: boolean; last_userscript_request_at?: number }

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
const sessions = ref<SessionSummary[]>([])
const selected = ref<SessionOpen | null>(null)
const messageSlots = ref<Array<Message | undefined>>([])
const sessionSearchHits = ref<SearchHit[]>([])
const branchOverview = ref<BranchOverview | null>(null)
const branchesLoading = ref(false)
const branchesError = ref('')
const backgroundLoadFailed = ref(false)
const messageListRef = ref<HTMLElement | null>(null)
const loading = ref(false)
const detailLoading = ref(false)
const error = ref('')
const query = ref('')
const committedQuery = ref('')
const platform = ref('')
const dateFrom = ref('')
const dateTo = ref('')
const showFilters = ref(false)
const showSettings = ref(false)
const showClosePrompt = ref(false)
const showDeletePrompt = ref(false)
const showDetailMenu = ref(false)
const showSessionInfo = ref(false)
const pendingCloseBehavior = ref<'hide_to_tray' | 'exit' | null>(null)
const detailMode = ref<'conversation' | 'branches'>('conversation')
const expandedThinking = ref(new Set<string>())
const settings = ref<SettingsModel>({ setup_complete: false, secret_enabled: false, allowed_origins: [], close_behavior: 'ask', tray_click_behavior: 'show_menu', theme: 'system' })
const originText = ref('')
const total = ref(0)
const page = ref(0)
const apiStatus = ref<ApiStatus>({ service: { state: 'starting' }, userscript_connected: false })
const secretCopied = ref(false)
const sessionPaneWidth = ref(520)
const resizingPanes = ref(false)
const searchElapsed = ref<number | null>(null)
const searchHitIndex = ref(-1)
const loopSearch = ref(false)
const toast = ref('')
const activeBranchNode = ref('')
const contextMenu = ref({ visible: false, x: 0, y: 0, selectedText: '' })
const sidebarCollapsed = ref(loadSidebarCollapsed())
const pageSize = 100
const clickDebounceMs = 250
let statusTimer: number | undefined
let unlistenCloseRequest: UnlistenFn | undefined
let resizeStartX = 0
let resizeStartWidth = 0
let toastTimer: number | undefined
let mermaidRenderVersion = 0
let systemThemeQuery: MediaQueryList | undefined
let savedThemeBeforeSettings: SettingsModel['theme'] = 'system'
let sessionLoadGeneration = 0
let backgroundLoadTimer: number | undefined
let readingPositionTimer: number | undefined
let branchLoadGeneration = 0
let searchLoadGeneration = 0
const lastControlClicks = new WeakMap<Element, number>()
const pendingMessageBatches = new Map<string, Promise<boolean>>()

const displayedMessageSeqs = computed(() => {
  if (branchOverview.value && activeBranchNode.value) {
    return branchConversation(branchOverview.value.nodes, activeBranchNode.value).map((node) => node.seq)
  }
  return Array.from({ length: selected.value?.message_count ?? 0 }, (_, seq) => seq)
})
const displayedSeqIndexes = computed(() => new Map(displayedMessageSeqs.value.map((seq, index) => [seq, index])))
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

function clearSelectedSession() {
  sessionLoadGeneration += 1
  branchLoadGeneration += 1
  searchLoadGeneration += 1
  window.clearTimeout(backgroundLoadTimer)
  selected.value = null
  messageSlots.value = []
  sessionSearchHits.value = []
  branchOverview.value = null
  branchesLoading.value = false
  branchesError.value = ''
  expandedThinking.value = new Set()
}

function effectiveTheme(preference = settings.value.theme) {
  return preference === 'system' ? (systemThemeQuery?.matches ? 'dark' : 'light') : preference
}

function commitTheme(preference: SettingsModel['theme'], animate = true) {
  const theme = effectiveTheme(preference)
  if (document.documentElement.dataset.theme === theme) {
    document.documentElement.style.colorScheme = theme
    return
  }
  const apply = () => {
    document.documentElement.dataset.theme = theme
    document.documentElement.style.colorScheme = theme
    mermaidInstance = null
  }
  const documentWithTransitions = document as Document & { startViewTransition?: (callback: () => void) => void }
  if (animate && documentWithTransitions.startViewTransition) documentWithTransitions.startViewTransition(apply)
  else if (animate) {
    document.documentElement.classList.add('theme-transition')
    void document.documentElement.offsetWidth
    apply()
    window.setTimeout(() => document.documentElement.classList.remove('theme-transition'), 360)
  } else apply()
  window.setTimeout(() => {
    document.querySelectorAll<HTMLElement>('.mermaid-diagram').forEach((element) => {
      element.removeAttribute('data-rendered')
    })
    void renderMermaidDiagrams()
  }, animate ? 180 : 0)
}

function previewTheme(theme: SettingsModel['theme']) {
  settings.value.theme = theme
  commitTheme(theme)
}

function closeSettings(save = false) {
  if (!save) {
    settings.value.theme = savedThemeBeforeSettings
    commitTheme(savedThemeBeforeSettings)
  }
  showSettings.value = false
}

function setSidebarCollapsed(collapsed: boolean) {
  sidebarCollapsed.value = collapsed
  saveSidebarCollapsed(collapsed)
}

const filtered = computed(() => Boolean(query.value || platform.value || dateFrom.value || dateTo.value))
const hasBranches = computed(() => selected.value?.has_branches ?? false)
const statusLabel = computed(() => apiStatus.value.service.state === 'running' ? '同步服务运行中' : apiStatus.value.service.state === 'failed' ? '同步服务异常' : '同步服务启动中')
const sourceIndex = computed(() => ['', 'deepseek', 'doubao', 'kimi'].indexOf(platform.value))
const sourceAccent = computed(() => ({ deepseek: '#4d8fe8', doubao: '#e05c62', kimi: '#39a878' } as Record<string, string>)[platform.value] ?? '#f5f7f7')
const selectedMatches = computed<SearchMatch[]>(() => expandSearchHits(sessionSearchHits.value)
  .filter((match) => displayedSeqIndexes.value.has(match.seq)))
const loadedMessageCount = computed(() => messageSlots.value.reduce((count, message) => count + (message ? 1 : 0), 0))
const compactReferences = computed(() => new Map((selected.value?.references ?? []).map((reference) => [reference.cite_index, reference])))
function epoch(value: string, end = false) {
  if (!value) return null
  return String(new Date(`${value}T${end ? '23:59:59' : '00:00:00'}`).getTime() / 1000)
}

async function loadSessions(reset = true) {
  const started = performance.now()
  loading.value = true
  error.value = ''
  if (reset) page.value = 0
  if (reset) committedQuery.value = query.value.trim()
  try {
    const result = await invoke<{ sessions: SessionSummary[]; total: number }>('search_sessions', {
      query: {
        q: committedQuery.value || null,
        platform: platform.value || null,
        date_from: epoch(dateFrom.value),
        date_to: epoch(dateTo.value, true),
        limit: pageSize,
        offset: page.value * pageSize,
      },
    })
    sessions.value = reset ? result.sessions : [...sessions.value, ...result.sessions]
    total.value = result.total
    searchElapsed.value = committedQuery.value ? performance.now() - started : null
    if (reset && selected.value && !result.sessions.some((item) => item.id === selected.value?.id)) {
      persistReadingPosition()
      clearSelectedSession()
    }
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

async function selectSession(id: string) {
  const generation = ++sessionLoadGeneration
  window.clearTimeout(backgroundLoadTimer)
  persistReadingPosition()
  detailLoading.value = true
  error.value = ''
  sessionSearchHits.value = []
  branchOverview.value = null
  branchesError.value = ''
  try {
    const readingPosition = loadReadingPosition(id)
    const opened = await invoke<SessionOpen>('open_session', { id, anchorSeq: readingPosition?.seq ?? null })
    if (generation !== sessionLoadGeneration) return
    let overview: BranchOverview | null = null
    if (opened.has_branches) {
      try {
        overview = await invoke<BranchOverview>('get_session_branches', { id })
      } catch (reason) {
        branchesError.value = String(reason)
      }
    }
    if (generation !== sessionLoadGeneration) return
    selected.value = opened
    branchOverview.value = overview
    backgroundLoadFailed.value = false
    messageSlots.value = mergeMessageBatch(Array.from({ length: opened.message_count }), opened.messages)
    detailMode.value = 'conversation'
    expandedThinking.value = new Set()
    searchHitIndex.value = -1
    activeBranchNode.value = overview?.default_leaf_node_id ?? ''
    await nextTick()
    const targetSeq = readingPosition && displayedSeqIndexes.value.has(readingPosition.seq)
      ? readingPosition.seq
      : displayedMessageSeqs.value[0] ?? opened.start_seq
    messageVirtualizer.value.scrollToIndex(displayedSeqIndexes.value.get(targetSeq) ?? 0, { align: 'start' })
    await nextTick()
    if (readingPosition?.offset) messageListRef.value?.scrollBy({ top: readingPosition.offset })
    void loadSearchHits(generation)
    scheduleBackgroundLoad(generation)
  } catch (reason) {
    if (generation !== sessionLoadGeneration) return
    error.value = String(reason)
  } finally {
    if (generation === sessionLoadGeneration) detailLoading.value = false
  }
}

async function fetchMessageBatch(startSeq: number, generation = sessionLoadGeneration) {
  if (!selected.value || generation !== sessionLoadGeneration) return false
  const normalizedStart = Math.max(0, Math.floor(startSeq / 50) * 50)
  const sessionId = selected.value.id
  const batchKey = `${sessionId}:${normalizedStart}`
  const pending = pendingMessageBatches.get(batchKey)
  if (pending) return pending
  const request = (async () => {
    const messages = await invoke<Message[]>('get_session_messages', { id: sessionId, startSeq: normalizedStart, limit: 50 })
    if (generation !== sessionLoadGeneration || selected.value?.id !== sessionId) return false
    messageSlots.value = mergeMessageBatch(messageSlots.value, messages)
    return messages.length > 0
  })().finally(() => pendingMessageBatches.delete(batchKey))
  pendingMessageBatches.set(batchKey, request)
  return request
}

function nextMissingBatch() {
  const index = messageSlots.value.findIndex((message) => !message)
  return index < 0 ? null : Math.floor(index / 50) * 50
}

function scheduleBackgroundLoad(generation: number) {
  window.clearTimeout(backgroundLoadTimer)
  backgroundLoadTimer = window.setTimeout(async () => {
    if (generation !== sessionLoadGeneration) return
    const startSeq = nextMissingBatch()
    if (startSeq === null) return
    try {
      await fetchMessageBatch(startSeq, generation)
      scheduleBackgroundLoad(generation)
    } catch (reason) {
      if (generation === sessionLoadGeneration) {
        backgroundLoadFailed.value = true
        error.value = `后台加载对话失败：${String(reason)}`
      }
    }
  }, 16)
}

function retryBackgroundLoad() {
  backgroundLoadFailed.value = false
  error.value = ''
  scheduleBackgroundLoad(sessionLoadGeneration)
}

async function ensureMessageLoaded(seq: number) {
  if (messageSlots.value[seq]) return
  await fetchMessageBatch(Math.floor(seq / 50) * 50)
}

async function selectBranch(branch: BranchNode) {
  activeBranchNode.value = branch.node_id
  searchHitIndex.value = -1
  detailMode.value = 'conversation'
  await ensureMessageLoaded(branch.seq)
  await nextTick()
  messageVirtualizer.value.measure()
  messageVirtualizer.value.scrollToIndex(displayedSeqIndexes.value.get(branch.seq) ?? 0, { align: 'center', behavior: 'smooth' })
}

function escapeRegExp(value: string) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

function highlightRenderedHtml(html: string) {
  const needle = committedQuery.value
  if (!needle) return html
  const pattern = new RegExp(escapeRegExp(needle), 'gi')
  return html.split(/(<[^>]+>)/g).map((part) => part.startsWith('<') ? part : part.replace(pattern, (match) => `<mark class="search-hit">${match}</mark>`)).join('')
}

function highlightTitle(value: string) {
  return highlightRenderedHtml(escapeTitle(value))
}

async function navigateSearch(direction: number) {
  if (!selectedMatches.value.length) return
  let next = searchHitIndex.value + direction
  if (next < 0 || next >= selectedMatches.value.length) {
    if (!loopSearch.value) return
    next = next < 0 ? selectedMatches.value.length - 1 : 0
    toast.value = '已循环到当前对话的另一端'
    window.clearTimeout(toastTimer)
    toastTimer = window.setTimeout(() => { toast.value = '' }, 1800)
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

async function loadSearchHits(generation = sessionLoadGeneration) {
  const searchGeneration = ++searchLoadGeneration
  if (!selected.value || !committedQuery.value) {
    sessionSearchHits.value = []
    return
  }
  const hits = await invoke<SearchHit[]>('search_session_hits', { id: selected.value.id, query: committedQuery.value })
  if (generation === sessionLoadGeneration && searchGeneration === searchLoadGeneration) sessionSearchHits.value = hits
}

async function loadBranches() {
  if (!selected.value || branchOverview.value || branchesLoading.value) return
  const generation = ++branchLoadGeneration
  branchesLoading.value = true
  branchesError.value = ''
  try {
    const overview = await invoke<BranchOverview>('get_session_branches', { id: selected.value.id })
    if (generation !== branchLoadGeneration) return
    branchOverview.value = overview
    activeBranchNode.value ||= overview.default_leaf_node_id
  } catch (reason) {
    if (generation === branchLoadGeneration) branchesError.value = String(reason)
  } finally {
    if (generation === branchLoadGeneration) branchesLoading.value = false
  }
}

function showBranches() {
  detailMode.value = 'branches'
  void loadBranches()
}

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

async function removeSession() {
  if (!selected.value) return
  try {
    await invoke('delete_session', { id: selected.value.id })
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
    await invoke('import_deepseek_zip', { path })
    await loadSessions()
  } catch (reason) {
    error.value = String(reason)
  } finally {
    loading.value = false
  }
}

async function openSettings() {
  settings.value = await invoke('get_settings')
  savedThemeBeforeSettings = settings.value.theme
  originText.value = settings.value.allowed_origins.join('\n')
  secretCopied.value = false
  showSettings.value = true
}

async function saveSettings() {
  settings.value.allowed_origins = originText.value.split('\n').map((value) => value.trim()).filter(Boolean)
  settings.value.setup_complete = true
  try {
    settings.value = await invoke('save_settings', { settings: settings.value })
    savedThemeBeforeSettings = settings.value.theme
    closeSettings(true)
  } catch (reason) {
    error.value = String(reason)
  }
}

async function rotateSecret() {
  settings.value = await invoke('rotate_secret')
  secretCopied.value = false
}

async function copySecret() {
  if (!settings.value.secret) return
  await navigator.clipboard.writeText(settings.value.secret)
  secretCopied.value = true
}

async function changeDataDirectory() {
  const path = await open({ directory: true, multiple: false, title: '选择数据保存目录' })
  if (typeof path !== 'string') return
  if (!confirm('应用将把当前数据库复制到新目录并立即重启。是否继续？')) return
  try {
    await invoke('move_data_directory', { path })
  } catch (reason) {
    error.value = String(reason)
  }
}

async function confirmClose() {
  if (!pendingCloseBehavior.value) return
  try {
    await invoke('confirm_close_behavior', { behavior: pendingCloseBehavior.value })
    showClosePrompt.value = false
  } catch (reason) {
    error.value = String(reason)
  }
}

function cancelClose() {
  showClosePrompt.value = false
  pendingCloseBehavior.value = null
}

function resetFilters() {
  query.value = ''
  platform.value = ''
  dateFrom.value = ''
  dateTo.value = ''
  void loadSessions()
}

function selectPlatform(value: string) {
  platform.value = value
  void loadSessions()
}

function startPaneResize(event: PointerEvent) {
  resizingPanes.value = true
  resizeStartX = event.clientX
  resizeStartWidth = sessionPaneWidth.value
  const target = event.currentTarget as HTMLElement
  target.setPointerCapture(event.pointerId)
}

function resizePanes(event: PointerEvent) {
  if (!resizingPanes.value) return
  const workspaceWidth = document.querySelector<HTMLElement>('.content-grid')?.clientWidth ?? 1000
  sessionPaneWidth.value = Math.min(Math.max(resizeStartWidth + event.clientX - resizeStartX, 340), workspaceWidth - 380)
}

function stopPaneResize() {
  resizingPanes.value = false
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
  apiStatus.value = await invoke('get_api_status')
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
  systemThemeQuery = window.matchMedia('(prefers-color-scheme: dark)')
  systemThemeQuery.addEventListener('change', handleSystemThemeChange)
  commitTheme('system', false)
  window.addEventListener('keydown', handleContextMenuKey)
  document.addEventListener('click', preventRapidControlClick, true)
  document.addEventListener('scroll', hideContextMenu, true)
  unlistenCloseRequest = await listen('close-behavior-requested', () => {
    pendingCloseBehavior.value = null
    showClosePrompt.value = true
  })
  settings.value = await invoke('get_settings')
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
onBeforeUnmount(() => {
  persistReadingPosition()
  sessionLoadGeneration += 1
  branchLoadGeneration += 1
  systemThemeQuery?.removeEventListener('change', handleSystemThemeChange)
  window.removeEventListener('keydown', handleContextMenuKey)
  document.removeEventListener('click', preventRapidControlClick, true)
  document.removeEventListener('scroll', hideContextMenu, true)
  window.clearInterval(statusTimer)
  window.clearTimeout(toastTimer)
  window.clearTimeout(backgroundLoadTimer)
  window.clearTimeout(readingPositionTimer)
  unlistenCloseRequest?.()
})

function handleSystemThemeChange() {
  if (settings.value.theme === 'system') commitTheme('system')
}
</script>

<template>
  <div :class="['app-frame', { 'sidebar-collapsed': sidebarCollapsed }]" @click="hidePopupMenus" @contextmenu="handleContextMenu">
    <aside class="sidebar" :aria-label="sidebarCollapsed ? '已折叠的来源导航' : '来源导航'">
      <div class="identity">
        <div class="identity-content">
          <div class="identity-mark"><MessageSquareText :size="20" /></div>
          <div class="identity-name"><strong>对话归档</strong><span>AI Chat Memory</span></div>
          <button class="sidebar-toggle sidebar-toggle-collapse" title="折叠侧边栏" aria-label="折叠侧边栏" @click="setSidebarCollapsed(true)"><PanelLeftClose :size="17" /></button>
        </div>
        <button class="sidebar-toggle sidebar-toggle-expand" title="展开侧边栏" aria-label="展开侧边栏" @click="setSidebarCollapsed(false)"><PanelLeftOpen :size="19" /></button>
      </div>

      <nav aria-label="主要导航">
        <button class="nav-item active" title="全部对话" aria-label="全部对话"><Inbox :size="17" /><span>全部对话</span><em>{{ total }}</em></button>
      </nav>

      <div class="sidebar-section">
        <p>来源</p>
        <div class="source-picker">
          <span class="source-highlight" :style="{ transform: `translateY(${sourceIndex * 34}px)`, '--source-accent': sourceAccent }"></span>
          <button :class="['source-item', { active: platform === '' }]" title="全部来源" aria-label="全部来源" @click="selectPlatform('')"><i class="source-glyph all">全</i><span>全部来源</span></button>
          <button :class="['source-item', { active: platform === 'deepseek' }]" title="DeepSeek" aria-label="DeepSeek" @click="selectPlatform('deepseek')"><i class="source-glyph deepseek">D</i><span>DeepSeek</span></button>
          <button :class="['source-item', { active: platform === 'doubao' }]" title="豆包" aria-label="豆包" @click="selectPlatform('doubao')"><i class="source-glyph doubao">豆</i><span>豆包</span></button>
          <button :class="['source-item', { active: platform === 'kimi' }]" title="Kimi" aria-label="Kimi" @click="selectPlatform('kimi')"><i class="source-glyph kimi">K</i><span>Kimi</span></button>
        </div>
      </div>

      <div class="sidebar-footer">
        <div class="service-state" :class="apiStatus.service.state">
          <span class="status-dot"></span>
          <div><strong>{{ statusLabel }}</strong><small :class="['connection-label', { connected: apiStatus.userscript_connected }]">{{ apiStatus.userscript_connected ? '网页脚本已连接' : '等待网页脚本连接' }}</small></div>
        </div>
        <button :class="['icon-button', 'settings-button', { connected: apiStatus.userscript_connected }]" title="设置" aria-label="设置" @click="openSettings"><Settings :size="18" /></button>
      </div>
    </aside>

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
        <div class="session-pane">
          <div class="table-head"><span>对话</span><span>来源</span><span>更新时间</span></div>
          <div v-if="loading && !sessions.length" class="loading-state"><LoaderCircle class="spinning" :size="22" /><span>正在读取对话</span></div>
          <button
            v-for="session in sessions"
            :key="session.id"
            :class="['session-row', { selected: selected?.id === session.id }]"
            @click="selectSession(session.id)"
          >
            <span class="session-title"><strong v-html="highlightTitle(session.title)"></strong></span>
            <span class="platform-cell"><i :class="session.platform"></i>{{ platformName(session.platform) }}</span>
            <time>{{ formatDate(session.updated_at, true) }}</time>
          </button>
          <div v-if="!loading && !sessions.length" class="empty-state">
            <Archive :size="30" />
            <strong>{{ filtered ? '没有匹配的对话' : '还没有对话记录' }}</strong>
            <span>{{ filtered ? '调整搜索或筛选条件后重试' : '同步 userscript 或导入 DeepSeek ZIP 后会显示在这里' }}</span>
          </div>
          <button v-if="sessions.length < total" class="load-more" :disabled="loading" @click="loadMore">{{ loading ? '加载中' : `加载更多（剩余 ${total-sessions.length} 条）` }}</button>
        </div>

        <div class="pane-resizer" role="separator" aria-label="调整对话列表和内容宽度" aria-orientation="vertical" tabindex="0" @pointerdown="startPaneResize" @pointermove="resizePanes" @pointerup="stopPaneResize" @pointercancel="stopPaneResize"></div>

        <aside class="detail-pane">
          <div v-if="detailLoading" class="loading-state"><LoaderCircle class="spinning" :size="22" /><span>正在打开对话</span></div>
          <template v-else-if="selected">
            <div class="detail-header">
              <div class="detail-title"><span class="platform-badge"><i :class="selected.platform"></i>{{ platformName(selected.platform) }}</span><h2>{{ selected.title || '未命名对话' }}</h2><p>{{ displayedMessageSeqs.length }} 条消息<span v-if="branchOverview && displayedMessageSeqs.length < selected.message_count"> · 共 {{ selected.message_count }} 个版本节点</span> · {{ formatDate(selected.updated_at) }}<span v-if="loadedMessageCount < selected.message_count" class="load-progress"> · 已加载 {{ loadedMessageCount }}/{{ selected.message_count }}</span><button v-if="backgroundLoadFailed" class="inline-retry" @click="retryBackgroundLoad">重试</button></p></div>
              <div class="detail-actions" @click.stop>
                <button class="icon-button" title="更多操作" aria-haspopup="menu" :aria-expanded="showDetailMenu" @click="toggleDetailMenu"><MoreHorizontal :size="19" /></button>
                <div v-if="showDetailMenu" class="detail-menu" role="menu">
                  <button role="menuitem" @click="showSessionInfo=true; showDetailMenu=false">对话详细信息</button>
                  <button class="danger" role="menuitem" @click="showDeletePrompt=true; showDetailMenu=false"><Trash2 :size="14" />删除对话</button>
                </div>
              </div>
            </div>
            <div v-if="hasBranches" class="segmented-control">
              <button :class="{ active: detailMode === 'conversation' }" @click="detailMode='conversation'"><MessageSquareText :size="15" />对话</button>
              <button :class="{ active: detailMode === 'branches' }" @click="showBranches"><GitBranch :size="15" />分支预览</button>
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
                    class="virtual-message"
                    :data-index="virtualMessage.index"
                    :style="{ transform: `translateY(${virtualMessage.start}px)` }"
                  >
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

    <Transition name="settings-modal">
      <div v-if="showSettings" class="dialog-backdrop" @click.self="closeSettings()">
        <section class="settings-dialog" role="dialog" aria-modal="true" aria-labelledby="settings-title">
        <header><div><h2 id="settings-title">应用设置</h2><p>配置界面、桌面行为和本地同步服务</p></div></header>
        <div class="settings-content">
          <section class="setting-group theme-setting">
            <div><h3>外观</h3><p>选择应用配色，跟随系统会随 Windows 主题自动切换。</p></div>
            <div class="theme-options" role="radiogroup" aria-label="应用主题">
              <button :class="{ active: settings.theme === 'system' }" role="radio" :aria-checked="settings.theme === 'system'" @click="previewTheme('system')"><Monitor :size="16" /><span>跟随系统</span></button>
              <button :class="{ active: settings.theme === 'light' }" role="radio" :aria-checked="settings.theme === 'light'" @click="previewTheme('light')"><Sun :size="16" /><span>亮色</span></button>
              <button :class="{ active: settings.theme === 'dark' }" role="radio" :aria-checked="settings.theme === 'dark'" @click="previewTheme('dark')"><Moon :size="16" /><span>深色</span></button>
            </div>
          </section>
          <section class="setting-group">
            <div class="setting-row"><div><h3>数据保存位置</h3><p class="path-value">{{ settings.data_directory || '系统默认应用数据目录' }}</p></div><button class="secondary-button" @click="changeDataDirectory">更改位置</button></div>
          </section>
          <section class="setting-group behavior-settings">
            <label><span>关闭窗口后</span><select v-model="settings.close_behavior"><option value="ask">下次关闭时询问</option><option value="hide_to_tray">隐藏到系统托盘</option><option value="exit">退出应用</option></select></label>
            <label><span>点击托盘图标</span><select v-model="settings.tray_click_behavior"><option value="show_menu">弹出托盘菜单</option><option value="open_window">打开主界面</option><option value="no_action">不执行操作</option></select></label>
          </section>
          <section class="setting-group">
            <div class="setting-heading"><ShieldCheck :size="18" /><div><h3>允许的网页来源</h3><p>每行填写一个完整的 HTTP 或 HTTPS Origin，不支持通配符。</p></div></div>
            <textarea v-model="originText" spellcheck="false" aria-label="Origin 白名单"></textarea>
          </section>
          <section class="setting-group">
            <div class="setting-row"><div><h3>同步密钥</h3><p>要求 userscript 携带额外密钥访问本地服务。</p></div><label class="switch"><input v-model="settings.secret_enabled" type="checkbox" /><span></span></label></div>
            <div v-if="settings.secret_enabled" class="secret-field"><code>{{ settings.secret || '保存设置后自动生成' }}</code><button class="icon-button" :title="secretCopied ? '已复制' : '复制密钥'" :disabled="!settings.secret" @click="copySecret"><Check v-if="secretCopied" :size="17" /><Clipboard v-else :size="17" /></button><button class="secondary-button compact" @click="rotateSecret">重新生成</button></div>
          </section>
        </div>
        <footer><button class="secondary-button" @click="closeSettings()">取消</button><button class="primary-button" @click="saveSettings">保存设置</button></footer>
        </section>
      </div>
    </Transition>

    <Transition name="settings-modal">
      <div v-if="showSessionInfo && selected" class="dialog-backdrop" @click.self="showSessionInfo=false">
        <section class="info-dialog" role="dialog" aria-modal="true" aria-labelledby="info-title">
          <header><h2 id="info-title">对话详细信息</h2><button class="icon-button" title="关闭" @click="showSessionInfo=false"><X :size="18" /></button></header>
          <dl><dt>标题</dt><dd>{{ selected.title || '未命名对话' }}</dd><dt>来源</dt><dd>{{ platformName(selected.platform) }}</dd><dt>来源会话 ID</dt><dd class="identifier">{{ selected.platform_session_id }}</dd><dt>创建时间</dt><dd>{{ formatDate(selected.created_at) }}</dd><dt>更新时间</dt><dd>{{ formatDate(selected.updated_at) }}</dd><dt>消息数量</dt><dd>{{ selected.message_count }}</dd></dl>
        </section>
      </div>
    </Transition>

    <Transition name="settings-modal">
      <div v-if="showDeletePrompt && selected" class="dialog-backdrop" @click.self="showDeletePrompt=false">
        <section class="delete-dialog" role="alertdialog" aria-modal="true" aria-labelledby="delete-title">
          <header><div><h2 id="delete-title">删除对话</h2><p>“{{ selected.title || '未命名对话' }}”及其全部消息将被永久删除，此操作无法撤销。</p></div></header>
          <footer><button class="secondary-button" @click="showDeletePrompt=false">取消</button><button class="danger-button" @click="removeSession"><Trash2 :size="15" />确认删除</button></footer>
        </section>
      </div>
    </Transition>

    <Transition name="settings-modal">
      <div v-if="showClosePrompt" class="dialog-backdrop close-prompt-backdrop">
        <section class="close-prompt" role="alertdialog" aria-modal="true" aria-labelledby="close-prompt-title">
          <header><h2 id="close-prompt-title">关闭对话归档</h2><p>请选择关闭窗口后要执行的操作。你的选择会保存，也可以稍后在设置中修改。</p></header>
          <div class="close-options">
            <label :class="{ selected: pendingCloseBehavior === 'hide_to_tray' }"><input type="checkbox" :checked="pendingCloseBehavior === 'hide_to_tray'" @change="pendingCloseBehavior='hide_to_tray'" /><span><strong>退出到托盘</strong><small>隐藏主窗口，本地同步服务继续运行。</small></span></label>
            <label :class="{ selected: pendingCloseBehavior === 'exit' }"><input type="checkbox" :checked="pendingCloseBehavior === 'exit'" @change="pendingCloseBehavior='exit'" /><span><strong>完全关闭</strong><small>退出应用并停止本地同步服务。</small></span></label>
          </div>
          <footer><button class="secondary-button" @click="cancelClose">取消</button><button class="primary-button" :disabled="!pendingCloseBehavior" @click="confirmClose">确认</button></footer>
        </section>
      </div>
    </Transition>
    <div v-if="contextMenu.visible" class="context-menu" role="menu" :style="{ left: `${contextMenu.x}px`, top: `${contextMenu.y}px` }" @click.stop>
      <button role="menuitem" :disabled="!contextMenu.selectedText" @click="copyContextSelection"><Copy :size="15" /><span>复制</span><kbd>Ctrl+C</kbd></button>
      <button role="menuitem" @click="selectConversationContent"><Clipboard :size="15" /><span>全选对话内容</span><kbd>Ctrl+A</kbd></button>
    </div>
    <Transition name="toast"><div v-if="toast" class="toast" role="status">{{ toast }}</div></Transition>
  </div>
</template>
