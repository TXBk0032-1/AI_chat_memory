<script setup lang="ts">
import { Check, Clipboard, FolderOpen, Monitor, Moon, RefreshCw, ShieldCheck, Sun } from 'lucide-vue-next'
import type {
  ApiStatus,
  EmbeddingBackendKind,
  LocalEmbeddingDevice,
  LocalEmbeddingDType,
  ModelDownloadProgress,
  ReindexProgress,
  SearchMode,
  SemanticRuntimeStatus,
  SettingsModel,
  ThemePreference,
} from '../desktop-api'

defineProps<{
  visible: boolean
  secretCopied: boolean
  mcpConfigCopied?: boolean
  apiStatus?: ApiStatus | null
  semanticStatus?: SemanticRuntimeStatus | null
  semanticBusy?: boolean
  downloadProgress?: ModelDownloadProgress | null
  reindexProgress?: ReindexProgress | null
}>()
const settings = defineModel<SettingsModel>('settings', { required: true })
const originText = defineModel<string>('originText', { required: true })
const emit = defineEmits<{
  close: []
  save: []
  previewTheme: [theme: ThemePreference]
  changeDataDirectory: []
  copySecret: []
  rotateSecret: []
  copyMcpConfig: []
  checkEmbedding: []
  reindexSemantic: []
  downloadLocalModel: []
  importLocalModel: []
  cancelSemanticWork: []
}>()

function mcpStateLabel(status: ApiStatus | null | undefined): string {
  const state = status?.mcp?.state
  if (state === 'running') return '运行中'
  if (state === 'starting') return '启动中'
  if (state === 'failed') {
    const message = status?.mcp && 'message' in status.mcp ? status.mcp.message : undefined
    return message ? `异常：${message}` : '异常'
  }
  return '已停止'
}

function setBackend(backend: EmbeddingBackendKind) {
  settings.value.semantic_search.backend = backend
}

function setMode(mode: SearchMode) {
  settings.value.semantic_search.default_mode = mode
}

if (!settings.value.semantic_search.local.device) {
  settings.value.semantic_search.local.device = 'auto' as LocalEmbeddingDevice
}
if (!settings.value.semantic_search.local.dtype) {
  settings.value.semantic_search.local.dtype = 'auto' as LocalEmbeddingDType
}
</script>

<template>
  <Transition name="settings-modal">
    <div v-if="visible" class="dialog-backdrop" @click.self="emit('close')">
      <section class="settings-dialog" role="dialog" aria-modal="true" aria-labelledby="settings-title">
        <header><div><h2 id="settings-title">应用设置</h2><p>配置界面、桌面行为和本地同步服务</p></div></header>
        <div class="settings-content">
          <section class="setting-group theme-setting">
            <div><h3>外观</h3><p>选择应用配色，跟随系统会随 Windows 主题自动切换。</p></div>
            <div class="theme-options" role="radiogroup" aria-label="应用主题">
              <button :class="{ active: settings.theme === 'system' }" role="radio" :aria-checked="settings.theme === 'system'" @click="emit('previewTheme', 'system')"><Monitor :size="16" /><span>跟随系统</span></button>
              <button :class="{ active: settings.theme === 'light' }" role="radio" :aria-checked="settings.theme === 'light'" @click="emit('previewTheme', 'light')"><Sun :size="16" /><span>亮色</span></button>
              <button :class="{ active: settings.theme === 'dark' }" role="radio" :aria-checked="settings.theme === 'dark'" @click="emit('previewTheme', 'dark')"><Moon :size="16" /><span>深色</span></button>
            </div>
          </section>
          <section class="setting-group">
            <div class="setting-row"><div><h3>数据保存位置</h3><p class="path-value">{{ settings.data_directory || '系统默认应用数据目录' }}</p></div><button class="secondary-button" @click="emit('changeDataDirectory')">更改位置</button></div>
          </section>
          <section class="setting-group behavior-settings">
            <label><span>关闭窗口后</span><select v-model="settings.close_behavior"><option value="ask">下次关闭时询问</option><option value="hide_to_tray">隐藏到系统托盘</option><option value="exit">退出应用</option></select></label>
            <label><span>点击托盘图标</span><select v-model="settings.tray_click_behavior"><option value="show_menu">弹出托盘菜单</option><option value="open_window">打开主界面</option><option value="no_action">不执行操作</option></select></label>
          </section>
          <section class="setting-group">
            <div class="setting-row">
              <div>
                <h3>语义搜索</h3>
                <p>默认混合关键词与向量召回。可切换本地模型或远程 embedding 后端。</p>
              </div>
              <label class="switch"><input v-model="settings.semantic_search.enabled" type="checkbox" /><span></span></label>
            </div>
            <div class="semantic-settings" v-if="settings.semantic_search.enabled">
              <label>
                <span>默认模式</span>
                <select :value="settings.semantic_search.default_mode" @change="setMode(($event.target as HTMLSelectElement).value as SearchMode)">
                  <option value="hybrid">混合</option>
                  <option value="semantic">语义</option>
                  <option value="keyword">关键词</option>
                </select>
              </label>
              <label>
                <span>Embedding 后端</span>
                <select :value="settings.semantic_search.backend" @change="setBackend(($event.target as HTMLSelectElement).value as EmbeddingBackendKind)">
                  <option value="local">本地 Candle / BGE</option>
                  <option value="ollama">Ollama</option>
                  <option value="llama_cpp">llama.cpp</option>
                  <option value="openai_compatible">OpenAI 兼容</option>
                </select>
              </label>
              <template v-if="settings.semantic_search.backend === 'local'">
                <label><span>模型 ID</span><input v-model="settings.semantic_search.local.model" /></label>
                <label>
                  <span>计算设备</span>
                  <select v-model="settings.semantic_search.local.device">
                    <option value="auto">自动</option>
                    <option value="cuda">CUDA</option>
                    <option value="cpu">CPU</option>
                  </select>
                </label>
                <label>
                  <span>精度</span>
                  <select v-model="settings.semantic_search.local.dtype">
                    <option value="auto">自动</option>
                    <option value="f16">F16</option>
                    <option value="f32">F32</option>
                  </select>
                </label>
                <p class="path-value">本地目录：{{ semanticStatus?.local_model_path || settings.semantic_search.local.model_path || '尚未准备' }}</p>
                <p class="path-value">下载源：Hugging Face（{{ settings.semantic_search.local.model }}）</p>
                <p class="path-value">当前设备：{{ semanticStatus?.device || '未加载' }} · 精度 {{ semanticStatus?.dtype || '未加载' }}</p>
                <div class="setting-actions">
                  <button class="secondary-button compact" :disabled="semanticBusy && !!downloadProgress && downloadProgress.stage !== 'done' && downloadProgress.stage !== 'error' && downloadProgress.stage !== 'cancelled'" @click="emit('downloadLocalModel')">{{ semanticBusy && downloadProgress && downloadProgress.stage !== 'done' && downloadProgress.stage !== 'error' && downloadProgress.stage !== 'cancelled' ? '下载中…' : '下载模型' }}</button>
                  <button class="secondary-button compact" :disabled="semanticBusy" @click="emit('importLocalModel')"><FolderOpen :size="14" />导入本地模型</button>
                  <button v-if="semanticBusy && downloadProgress && downloadProgress.stage !== 'done' && downloadProgress.stage !== 'error' && downloadProgress.stage !== 'cancelled'" class="secondary-button compact" @click="emit('cancelSemanticWork')">取消下载</button>
                </div>
                <div v-if="downloadProgress" class="download-progress" :data-stage="downloadProgress.stage">
                  <div class="download-progress-meta">
                    <span>{{ downloadProgress.message }}</span>
                    <strong>{{ Math.round((downloadProgress.fraction || 0) * 100) }}%</strong>
                  </div>
                  <div class="download-progress-track" aria-hidden="true">
                    <i :style="{ width: `${Math.max(2, Math.round((downloadProgress.fraction || 0) * 100))}%` }"></i>
                  </div>
                  <p v-if="downloadProgress.file" class="path-value">文件 {{ downloadProgress.file_index + 1 }}/{{ downloadProgress.file_count }}：{{ downloadProgress.file }}</p>
                </div>
              </template>
              <template v-else-if="settings.semantic_search.backend === 'ollama'">
                <label><span>Base URL</span><input v-model="settings.semantic_search.ollama.base_url" /></label>
                <label><span>模型</span><input v-model="settings.semantic_search.ollama.model" /></label>
                <label><span>维度（可空，自动探测）</span><input v-model.number="settings.semantic_search.ollama.dimensions" type="number" min="0" placeholder="自动" /></label>
              </template>
              <template v-else-if="settings.semantic_search.backend === 'llama_cpp'">
                <label><span>Base URL</span><input v-model="settings.semantic_search.llama_cpp.base_url" /></label>
                <label><span>模型</span><input v-model="settings.semantic_search.llama_cpp.model" /></label>
                <label><span>API Key</span><input v-model="settings.semantic_search.llama_cpp.api_key" /></label>
                <label><span>维度（可空，自动探测）</span><input v-model.number="settings.semantic_search.llama_cpp.dimensions" type="number" min="0" placeholder="自动" /></label>
              </template>
              <template v-else>
                <label><span>Base URL</span><input v-model="settings.semantic_search.openai_compatible.base_url" /></label>
                <label><span>模型</span><input v-model="settings.semantic_search.openai_compatible.model" /></label>
                <label><span>API Key</span><input v-model="settings.semantic_search.openai_compatible.api_key" /></label>
                <label><span>维度（可空，自动探测）</span><input v-model.number="settings.semantic_search.openai_compatible.dimensions" type="number" min="0" placeholder="自动" /></label>
              </template>
              <p class="path-value">
                状态：{{ semanticStatus?.status || 'unknown' }}
                · 就绪 {{ semanticStatus?.ready_chunks ?? 0 }}
                · 待索引 {{ semanticStatus?.pending_chunks ?? 0 }}
                <template v-if="semanticStatus?.message"> · {{ semanticStatus.message }}</template>
              </p>
              <div class="setting-actions">
                <button class="secondary-button compact" :disabled="semanticBusy" @click="emit('checkEmbedding')">测试后端</button>
                <button class="secondary-button compact" :disabled="!!(semanticBusy && reindexProgress && reindexProgress.stage !== 'done' && reindexProgress.stage !== 'error' && reindexProgress.stage !== 'cancelled')" @click="emit('reindexSemantic')">
                  <RefreshCw :size="14" :class="{ spinning: semanticBusy && reindexProgress && reindexProgress.stage !== 'done' && reindexProgress.stage !== 'error' && reindexProgress.stage !== 'cancelled' }" />
                  {{ semanticBusy && reindexProgress && reindexProgress.stage !== 'done' && reindexProgress.stage !== 'error' && reindexProgress.stage !== 'cancelled' ? '重建中…' : '重建索引' }}
                </button>
                <button v-if="semanticBusy && reindexProgress && reindexProgress.stage !== 'done' && reindexProgress.stage !== 'error' && reindexProgress.stage !== 'cancelled'" class="secondary-button compact" @click="emit('cancelSemanticWork')">取消编码</button>
              </div>
              <div v-if="reindexProgress" class="download-progress" :data-stage="reindexProgress.stage">
                <div class="download-progress-meta">
                  <span>{{ reindexProgress.message }}</span>
                  <strong>{{ Math.round((reindexProgress.fraction || 0) * 100) }}%</strong>
                </div>
                <div class="download-progress-track" aria-hidden="true">
                  <i :style="{ width: `${Math.max(2, Math.round((reindexProgress.fraction || 0) * 100))}%` }"></i>
                </div>
                <p class="path-value">
                  会话 {{ reindexProgress.processed_sessions }}/{{ reindexProgress.total_sessions }}
                  · 就绪 {{ reindexProgress.ready_chunks }}
                  · 待处理 {{ reindexProgress.pending_chunks }}
                </p>
              </div>
            </div>
          </section>
          <section class="setting-group">
            <div class="setting-heading"><ShieldCheck :size="18" /><div><h3>允许的网页来源</h3><p>每行填写一个完整的 HTTP 或 HTTPS Origin，不支持通配符。</p></div></div>
            <textarea v-model="originText" spellcheck="false" aria-label="Origin 白名单"></textarea>
          </section>
          <section class="setting-group">
            <div class="setting-row"><div><h3>同步密钥</h3><p>要求 userscript 携带额外密钥访问本地服务。</p></div><label class="switch"><input v-model="settings.secret_enabled" type="checkbox" /><span></span></label></div>
            <div v-if="settings.secret_enabled" class="secret-field"><code>{{ settings.secret || '保存设置后自动生成' }}</code><button class="icon-button" :title="secretCopied ? '已复制' : '复制密钥'" :disabled="!settings.secret" @click="emit('copySecret')"><Check v-if="secretCopied" :size="17" /><Clipboard v-else :size="17" /></button><button class="secondary-button compact" @click="emit('rotateSecret')">重新生成</button></div>
          </section>
          <section class="setting-group">
            <div class="setting-row">
              <div>
                <h3>MCP</h3>
                <p>本机 Streamable HTTP；无密钥；同机进程可读对话；关闭立即停服。</p>
              </div>
              <label class="switch"><input v-model="settings.mcp_enabled" type="checkbox" /><span></span></label>
            </div>
            <p class="path-value">地址：{{ apiStatus?.mcp_url || 'http://127.0.0.1:19821/mcp' }}</p>
            <p class="path-value">状态：{{ mcpStateLabel(apiStatus) }}</p>
            <div class="setting-actions">
              <button class="secondary-button compact mcp-copy-button" :class="{ copied: mcpConfigCopied }" type="button" @click="emit('copyMcpConfig')">
                <span class="mcp-copy-button__icon" aria-hidden="true">
                  <Clipboard class="mcp-copy-button__clipboard" :size="14" />
                  <Check class="mcp-copy-button__check" :size="14" />
                </span>
                <span class="mcp-copy-button__label">
                  <span class="mcp-copy-button__label-default">复制客户端配置</span>
                  <span class="mcp-copy-button__label-success">已复制客户端配置</span>
                </span>
              </button>
            </div>
          </section>
        </div>
        <footer><button class="secondary-button" @click="emit('close')">取消</button><button class="primary-button" @click="emit('save')">保存设置</button></footer>
      </section>
    </div>
  </Transition>
</template>
