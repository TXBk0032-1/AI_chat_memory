<script setup lang="ts">
import { Check, Clipboard, FolderOpen, Monitor, Moon, RefreshCw, ShieldCheck, Sun } from 'lucide-vue-next'
import type {
  EmbeddingBackendKind,
  SearchMode,
  SemanticRuntimeStatus,
  SettingsModel,
  ThemePreference,
} from '../desktop-api'

defineProps<{
  visible: boolean
  secretCopied: boolean
  semanticStatus?: SemanticRuntimeStatus | null
  semanticBusy?: boolean
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
  checkEmbedding: []
  reindexSemantic: []
  downloadLocalModel: []
  importLocalModel: []
}>()

function setBackend(backend: EmbeddingBackendKind) {
  settings.value.semantic_search.backend = backend
}

function setMode(mode: SearchMode) {
  settings.value.semantic_search.default_mode = mode
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
                  <option value="local">本地 Candle / harrier</option>
                  <option value="ollama">Ollama</option>
                  <option value="llama_cpp">llama.cpp</option>
                  <option value="openai_compatible">OpenAI 兼容</option>
                </select>
              </label>
              <template v-if="settings.semantic_search.backend === 'local'">
                <label><span>模型 ID</span><input v-model="settings.semantic_search.local.model" /></label>
                <p class="path-value">本地目录：{{ semanticStatus?.local_model_path || settings.semantic_search.local.model_path || '尚未准备' }}</p>
                <div class="setting-actions">
                  <button class="secondary-button compact" :disabled="semanticBusy" @click="emit('downloadLocalModel')">下载模型</button>
                  <button class="secondary-button compact" :disabled="semanticBusy" @click="emit('importLocalModel')"><FolderOpen :size="14" />导入本地模型</button>
                </div>
              </template>
              <template v-else-if="settings.semantic_search.backend === 'ollama'">
                <label><span>Base URL</span><input v-model="settings.semantic_search.ollama.base_url" /></label>
                <label><span>模型</span><input v-model="settings.semantic_search.ollama.model" /></label>
              </template>
              <template v-else-if="settings.semantic_search.backend === 'llama_cpp'">
                <label><span>Base URL</span><input v-model="settings.semantic_search.llama_cpp.base_url" /></label>
                <label><span>模型</span><input v-model="settings.semantic_search.llama_cpp.model" /></label>
                <label><span>API Key</span><input v-model="settings.semantic_search.llama_cpp.api_key" /></label>
              </template>
              <template v-else>
                <label><span>Base URL</span><input v-model="settings.semantic_search.openai_compatible.base_url" /></label>
                <label><span>模型</span><input v-model="settings.semantic_search.openai_compatible.model" /></label>
                <label><span>API Key</span><input v-model="settings.semantic_search.openai_compatible.api_key" /></label>
              </template>
              <p class="path-value">
                状态：{{ semanticStatus?.status || 'unknown' }}
                · 就绪 {{ semanticStatus?.ready_chunks ?? 0 }}
                · 待索引 {{ semanticStatus?.pending_chunks ?? 0 }}
                <template v-if="semanticStatus?.message"> · {{ semanticStatus.message }}</template>
              </p>
              <div class="setting-actions">
                <button class="secondary-button compact" :disabled="semanticBusy" @click="emit('checkEmbedding')">测试后端</button>
                <button class="secondary-button compact" :disabled="semanticBusy" @click="emit('reindexSemantic')"><RefreshCw :size="14" />重建索引</button>
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
        </div>
        <footer><button class="secondary-button" @click="emit('close')">取消</button><button class="primary-button" @click="emit('save')">保存设置</button></footer>
      </section>
    </div>
  </Transition>
</template>
