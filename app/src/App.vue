<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
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
  Trash2,
  X,
} from 'lucide-vue-next'
import MarkdownIt from 'markdown-it'
import texmath from 'markdown-it-texmath'
import katex from 'katex'
import 'katex/dist/katex.min.css'
import './style.css'

type SessionSummary = {
  id: string
  platform: string
  platform_session_id: string
  title: string
  created_at?: string
  updated_at?: string
  imported_at?: string
}
type Message = {
  id: string
  role: string
  content: string
  metadata: Record<string, unknown>
  created_at?: string
  seq: number
}
type SessionDetail = SessionSummary & { messages: Message[]; raw_data?: unknown }
type SettingsModel = {
  setup_complete: boolean
  secret_enabled: boolean
  secret?: string
  allowed_origins: string[]
  migrated_legacy_database: boolean
  data_directory?: string
  close_behavior: 'ask' | 'hide_to_tray' | 'exit'
  tray_click_behavior: 'show_menu' | 'open_window' | 'no_action'
}
type ApiStatus = { service: { state: string; message?: string }; userscript_connected: boolean; last_userscript_request_at?: number }

const markdown = new MarkdownIt({ html: false, linkify: true, breaks: true }).use(texmath, {
  engine: katex,
  delimiters: ['dollars', 'brackets'],
})
const sessions = ref<SessionSummary[]>([])
const selected = ref<SessionDetail | null>(null)
const loading = ref(false)
const detailLoading = ref(false)
const error = ref('')
const query = ref('')
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
const settings = ref<SettingsModel>({ setup_complete: false, secret_enabled: false, allowed_origins: [], migrated_legacy_database: false, close_behavior: 'ask', tray_click_behavior: 'show_menu' })
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
const pageSize = 100
let statusTimer: number | undefined
let unlistenCloseRequest: UnlistenFn | undefined
let resizeStartX = 0
let resizeStartWidth = 0
let toastTimer: number | undefined

const filtered = computed(() => Boolean(query.value || platform.value || dateFrom.value || dateTo.value))
const hasBranches = computed(() => selected.value?.messages.some((message) => metadata(message, 'source') === 'deepseek_export') ?? false)
const statusLabel = computed(() => apiStatus.value.service.state === 'running' ? '同步服务运行中' : apiStatus.value.service.state === 'failed' ? '同步服务异常' : '同步服务启动中')
const sourceIndex = computed(() => ['', 'deepseek', 'doubao', 'kimi'].indexOf(platform.value))
const sourceAccent = computed(() => ({ deepseek: '#4d8fe8', doubao: '#e05c62', kimi: '#39a878' } as Record<string, string>)[platform.value] ?? '#f5f7f7')
const selectedMatches = computed(() => {
  const needle = query.value.trim().toLocaleLowerCase()
  if (!needle || !selected.value) return []
  return selected.value.messages.flatMap((message) => {
    const text = `${message.content}\n${metadata(message, 'thinking') || ''}`.toLocaleLowerCase()
    const count = text.split(needle).length - 1
    return Array.from({ length: count }, (_, index) => ({ messageId: message.id, index }))
  })
})
const branchPath = computed(() => {
  const path = new Set<string>()
  const messages = selected.value?.messages ?? []
  const byNode = new Map(messages.map((message) => [metadata(message, 'node_id'), message]))
  let node = activeBranchNode.value || metadata(messages[messages.length - 1], 'node_id') || ''
  while (node && !path.has(node)) {
    path.add(node)
    node = metadata(byNode.get(node)!, 'parent_node_id') || ''
  }
  return path
})
const sessionReferences = computed(() => collectReferences(selected.value?.raw_data))

function epoch(value: string, end = false) {
  if (!value) return null
  return String(new Date(`${value}T${end ? '23:59:59' : '00:00:00'}`).getTime() / 1000)
}

async function loadSessions(reset = true) {
  const started = performance.now()
  loading.value = true
  error.value = ''
  if (reset) page.value = 0
  try {
    const result = await invoke<{ sessions: SessionSummary[]; total: number }>('search_sessions', {
      query: {
        q: query.value || null,
        platform: platform.value || null,
        date_from: epoch(dateFrom.value),
        date_to: epoch(dateTo.value, true),
        limit: pageSize,
        offset: page.value * pageSize,
      },
    })
    sessions.value = reset ? result.sessions : [...sessions.value, ...result.sessions]
    total.value = result.total
    searchElapsed.value = query.value ? performance.now() - started : null
    if (reset && selected.value && !result.sessions.some((item) => item.id === selected.value?.id)) selected.value = null
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
  detailLoading.value = true
  error.value = ''
  try {
    selected.value = await invoke<SessionDetail>('get_session', { id })
    detailMode.value = 'conversation'
    expandedThinking.value = new Set()
    searchHitIndex.value = -1
    activeBranchNode.value = metadata(selected.value.messages[selected.value.messages.length - 1], 'node_id') || ''
  } catch (reason) {
    error.value = String(reason)
  } finally {
    detailLoading.value = false
  }
}

async function selectBranch(message: Message) {
  activeBranchNode.value = metadata(message, 'node_id') || ''
  detailMode.value = 'conversation'
  await nextTick()
  document.querySelector<HTMLElement>(`[data-message-id="${CSS.escape(message.id)}"]`)?.scrollIntoView({ behavior: 'smooth', block: 'center' })
}

function escapeRegExp(value: string) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

function highlightRenderedHtml(html: string) {
  const needle = query.value.trim()
  if (!needle) return html
  const pattern = new RegExp(escapeRegExp(needle), 'gi')
  return html.split(/(<[^>]+>)/g).map((part) => part.startsWith('<') ? part : part.replace(pattern, (match) => `<mark class="search-hit">${match}</mark>`)).join('')
}

function highlightTitle(value: string) {
  return highlightRenderedHtml(markdown.utils.escapeHtml(value || '未命名对话'))
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
  await nextTick()
  const hits = document.querySelectorAll<HTMLElement>('.conversation-view mark.search-hit')
  hits[next]?.scrollIntoView({ behavior: 'smooth', block: 'center' })
}

function toggleThinking(messageId: string) {
  const next = new Set(expandedThinking.value)
  if (next.has(messageId)) next.delete(messageId)
  else next.add(messageId)
  expandedThinking.value = next
}

async function removeSession() {
  if (!selected.value) return
  try {
    await invoke('delete_session', { id: selected.value.id })
    selected.value = null
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
  originText.value = settings.value.allowed_origins.join('\n')
  secretCopied.value = false
  showSettings.value = true
}

async function saveSettings() {
  settings.value.allowed_origins = originText.value.split('\n').map((value) => value.trim()).filter(Boolean)
  settings.value.setup_complete = true
  try {
    settings.value = await invoke('save_settings', { settings: settings.value })
    showSettings.value = false
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

async function migrateLegacy() {
  const path = await open({ multiple: false, filters: [{ name: 'SQLite 数据库', extensions: ['db', 'sqlite', 'sqlite3'] }] })
  if (typeof path !== 'string') return
  try {
    await invoke('migrate_legacy_database', { path })
    settings.value = await invoke('get_settings')
    await loadSessions()
  } catch (reason) {
    error.value = String(reason)
  }
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

function handleContextMenu(event: MouseEvent) {
  event.preventDefault()
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
  if (event.key === 'Escape') hideContextMenu()
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

function referenceValue(reference: unknown, keys: string[]) {
  if (!reference || typeof reference !== 'object') return ''
  const item = reference as Record<string, unknown>
  for (const key of keys) if (typeof item[key] === 'string') return item[key] as string
  return ''
}

function collectReferences(value: unknown) {
  const references = new Map<number, unknown>()
  const visit = (item: unknown) => {
    if (Array.isArray(item)) {
      item.forEach(visit)
      return
    }
    if (!item || typeof item !== 'object') return
    const record = item as Record<string, unknown>
    const url = referenceValue(record, ['url', 'link', 'href'])
    const citeIndex = typeof record.cite_index === 'number' ? record.cite_index : Number(record.cite_index)
    if (url && Number.isInteger(citeIndex) && citeIndex >= 0) references.set(citeIndex, record)
    Object.values(record).forEach(visit)
  }
  visit(value)
  return references
}

function resolveReference(index: number, message?: Message) {
  const messageReferences = Array.isArray(message?.metadata?.references) ? message.metadata.references : []
  return sessionReferences.value.get(index)
    ?? messageReferences.find((reference) => Number((reference as Record<string, unknown>)?.cite_index) === index)
    ?? messageReferences[index]
    ?? messageReferences[index - 1]
}

function render(value: string, message?: Message) {
  const source = (value || '')
    .replace(/\\\[([\s\S]*?)\\\]/g, (_, formula) => `\n$$${formula}$$\n`)
    .replace(/\\\((.+?)\\\)/g, (_, formula) => `$${formula}$`)
  return highlightRenderedHtml(markdown.render(source).replace(/\[reference:(\d+)\]/gi, (_match, rawIndex) => {
      const index = Number(rawIndex)
      const reference = resolveReference(index, message)
      const url = referenceValue(reference, ['url', 'link', 'href'])
      if (!/^https?:\/\//i.test(url)) return `<span class="reference-missing" title="该引用来源未随历史记录保存">[${index}]</span>`
      const title = referenceValue(reference, ['title', 'name']) || `引用 ${index}`
      const summary = referenceValue(reference, ['snippet', 'summary', 'description', 'content']).replace(/\s+/g, ' ').slice(0, 280)
      const safeTitle = markdown.utils.escapeHtml(title)
      const safeSummary = markdown.utils.escapeHtml(summary)
      return `<a class="reference-link" href="${markdown.utils.escapeHtml(url)}" target="_blank" rel="noopener noreferrer" data-summary="${safeSummary}">${safeTitle}</a>`
    }))
}

function metadata(message: Message, key: string) {
  return message.metadata?.[key] as string | undefined
}

function metadataArray(message: Message, key: string) {
  const value = message.metadata?.[key]
  return Array.isArray(value) ? value : []
}

function branchDepth(message: Message) {
  let depth = 0
  let parent = metadata(message, 'parent_node_id')
  const map = new Map(selected.value?.messages.map((item) => [metadata(item, 'node_id'), item]))
  while (parent && map.has(parent) && depth < 12) {
    depth += 1
    parent = metadata(map.get(parent)!, 'parent_node_id')
  }
  return depth
}

onMounted(async () => {
  window.addEventListener('keydown', handleContextMenuKey)
  document.addEventListener('scroll', hideContextMenu, true)
  unlistenCloseRequest = await listen('close-behavior-requested', () => {
    pendingCloseBehavior.value = null
    showClosePrompt.value = true
  })
  settings.value = await invoke('get_settings')
  await refreshApiStatus()
  statusTimer = window.setInterval(refreshApiStatus, 3000)
  await loadSessions()
})
onBeforeUnmount(() => {
  window.removeEventListener('keydown', handleContextMenuKey)
  document.removeEventListener('scroll', hideContextMenu, true)
  window.clearInterval(statusTimer)
  window.clearTimeout(toastTimer)
  unlistenCloseRequest?.()
})
</script>

<template>
  <div class="app-frame" @click="hideContextMenu" @contextmenu="handleContextMenu">
    <aside class="sidebar">
      <div class="identity">
        <div class="identity-mark"><MessageSquareText :size="20" /></div>
        <div><strong>对话归档</strong><span>AI Chat Memory</span></div>
      </div>

      <nav aria-label="主要导航">
        <button class="nav-item active"><Inbox :size="17" /><span>全部对话</span><em>{{ total }}</em></button>
      </nav>

      <div class="sidebar-section">
        <p>来源</p>
        <div class="source-picker">
          <span class="source-highlight" :style="{ transform: `translateY(${sourceIndex * 34}px)`, '--source-accent': sourceAccent }"></span>
          <button :class="['source-item', { active: platform === '' }]" @click="selectPlatform('')"><i class="all"></i><span>全部来源</span></button>
          <button :class="['source-item', { active: platform === 'deepseek' }]" @click="selectPlatform('deepseek')"><i class="deepseek"></i><span>DeepSeek</span></button>
          <button :class="['source-item', { active: platform === 'doubao' }]" @click="selectPlatform('doubao')"><i class="doubao"></i><span>豆包</span></button>
          <button :class="['source-item', { active: platform === 'kimi' }]" @click="selectPlatform('kimi')"><i class="kimi"></i><span>Kimi</span></button>
        </div>
      </div>

      <div class="sidebar-footer">
        <div class="service-state" :class="apiStatus.service.state">
          <span class="status-dot"></span>
          <div><strong>{{ statusLabel }}</strong><small :class="['connection-label', { connected: apiStatus.userscript_connected }]">{{ apiStatus.userscript_connected ? '网页脚本已连接' : '等待网页脚本连接' }}</small></div>
        </div>
        <button class="icon-button" title="设置" @click="openSettings"><Settings :size="18" /></button>
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
              <div class="detail-title"><span class="platform-badge"><i :class="selected.platform"></i>{{ platformName(selected.platform) }}</span><h2>{{ selected.title || '未命名对话' }}</h2><p>{{ selected.messages.length }} 条消息 · {{ formatDate(selected.updated_at) }}</p></div>
              <div class="detail-actions">
                <button class="icon-button" title="更多操作" aria-haspopup="menu" :aria-expanded="showDetailMenu" @click="showDetailMenu=!showDetailMenu"><MoreHorizontal :size="19" /></button>
                <div v-if="showDetailMenu" class="detail-menu" role="menu">
                  <button role="menuitem" @click="showSessionInfo=true; showDetailMenu=false">对话详细信息</button>
                  <button class="danger" role="menuitem" @click="showDeletePrompt=true; showDetailMenu=false"><Trash2 :size="14" />删除对话</button>
                </div>
              </div>
            </div>
            <div v-if="hasBranches" class="segmented-control">
              <button :class="{ active: detailMode === 'conversation' }" @click="detailMode='conversation'"><MessageSquareText :size="15" />对话</button>
              <button :class="{ active: detailMode === 'branches' }" @click="detailMode='branches'"><GitBranch :size="15" />分支</button>
            </div>
            <div v-if="query && detailMode === 'conversation'" class="search-navigation">
              <span>{{ selectedMatches.length ? `${Math.max(searchHitIndex + 1, 0)} / ${selectedMatches.length}` : '当前对话无正文命中' }}</span>
              <button class="icon-button" title="上一个命中" :disabled="!selectedMatches.length" @click="navigateSearch(-1)"><ArrowUp :size="15" /></button>
              <button class="icon-button" title="下一个命中" :disabled="!selectedMatches.length" @click="navigateSearch(1)"><ArrowDown :size="15" /></button>
              <label><input v-model="loopSearch" type="checkbox" />循环</label>
            </div>
            <div class="message-list" @click="openMarkdownLink">
              <div v-if="selectedMatches.length" class="search-scroll-markers" aria-hidden="true">
                <i v-for="(match, index) in selectedMatches" :key="`${match.messageId}-${index}`" :style="{ top: `${((selected?.messages.findIndex(item => item.id === match.messageId) ?? 0) + 0.5) / Math.max(selected?.messages.length ?? 1, 1) * 100}%` }"></i>
              </div>
              <Transition name="detail-camera" mode="out-in">
                <div v-if="detailMode === 'conversation'" key="conversation" class="conversation-view">
                  <article v-for="message in selected.messages" :key="message.id" :data-message-id="message.id" :class="['message-block', message.role]">
                    <div class="message-author"><span>{{ roleName(message.role) }}</span><time>{{ formatDate(message.created_at, true) }}</time></div>
                    <section v-if="metadata(message, 'thinking')" :class="['thinking', { open: expandedThinking.has(message.id) }]">
                      <button class="thinking-toggle" :aria-expanded="expandedThinking.has(message.id)" @click="toggleThinking(message.id)">查看思考过程</button>
                      <div class="thinking-reveal" :aria-hidden="!expandedThinking.has(message.id)"><div><div class="markdown" v-html="render(metadata(message, 'thinking') || '')"></div></div></div>
                    </section>
                    <div class="markdown" v-html="render(message.content, message)"></div>
                  </article>
                </div>
                <div v-else key="branches" class="branch-list">
                  <header><div><strong>对话分支</strong><span>{{ selected.messages.length }} 个节点</span></div><small>选择节点可返回对应消息</small></header>
                  <div class="branch-tree">
                    <button v-for="message in selected.messages" :key="message.id" :class="['branch-row', { current: activeBranchNode === metadata(message, 'node_id'), path: branchPath.has(metadata(message, 'node_id') || '') }]" :style="{ '--branch-depth': branchDepth(message) }" @click="selectBranch(message)">
                      <i class="branch-rail"></i><span class="branch-node"></span><div><strong>{{ roleName(message.role) }}</strong><p>{{ message.content || metadata(message, 'thinking') || '空消息' }}</p><small>{{ metadataArray(message, 'children_node_ids').length }} 个后续节点</small></div>
                    </button>
                  </div>
                </div>
              </Transition>
            </div>
          </template>
          <div v-else class="detail-placeholder"><MessageSquareText :size="34" /><strong>选择一条对话</strong><span>消息内容会显示在这里</span></div>
        </aside>
      </section>
    </main>

    <Transition name="settings-modal">
      <div v-if="showSettings" class="dialog-backdrop" @click.self="showSettings=false">
        <section class="settings-dialog" role="dialog" aria-modal="true" aria-labelledby="settings-title">
        <header><div><h2 id="settings-title">应用设置</h2><p>配置本地同步服务和数据迁移</p></div><button class="icon-button" title="关闭" @click="showSettings=false"><X :size="18" /></button></header>
        <div class="settings-content">
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
          <section class="setting-group migration-group">
            <div><h3>旧版数据</h3><p>{{ settings.migrated_legacy_database ? '旧版数据库已经迁移。' : '从旧版 Python 服务的 SQLite 数据库导入会话。' }}</p></div>
            <button v-if="!settings.migrated_legacy_database" class="secondary-button" @click="migrateLegacy">选择数据库</button>
          </section>
        </div>
        <footer><button class="secondary-button" @click="showSettings=false">取消</button><button class="primary-button" @click="saveSettings">保存设置</button></footer>
        </section>
      </div>
    </Transition>

    <Transition name="settings-modal">
      <div v-if="showSessionInfo && selected" class="dialog-backdrop" @click.self="showSessionInfo=false">
        <section class="info-dialog" role="dialog" aria-modal="true" aria-labelledby="info-title">
          <header><h2 id="info-title">对话详细信息</h2><button class="icon-button" title="关闭" @click="showSessionInfo=false"><X :size="18" /></button></header>
          <dl><dt>标题</dt><dd>{{ selected.title || '未命名对话' }}</dd><dt>来源</dt><dd>{{ platformName(selected.platform) }}</dd><dt>来源会话 ID</dt><dd class="identifier">{{ selected.platform_session_id }}</dd><dt>创建时间</dt><dd>{{ formatDate(selected.created_at) }}</dd><dt>更新时间</dt><dd>{{ formatDate(selected.updated_at) }}</dd><dt>消息数量</dt><dd>{{ selected.messages.length }}</dd></dl>
          <footer><button class="primary-button" @click="showSessionInfo=false">关闭</button></footer>
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
