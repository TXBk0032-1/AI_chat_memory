<script setup lang="ts">
import { getCurrentWindow } from '@tauri-apps/api/window'
import { Minus, Square, X } from 'lucide-vue-next'
import { onBeforeUnmount, onMounted } from 'vue'

const appWindow = getCurrentWindow()
let unlistenResize: (() => void) | undefined

async function syncMaximizedState() {
  const maximized = await appWindow.isMaximized()
  document.documentElement.classList.toggle('window-maximized', maximized)
}

function runWindowAction(action: () => Promise<void>) {
  void action().catch((error) => console.error('Window action failed', error))
}

onMounted(() => {
  void syncMaximizedState().catch((error) => console.error('Failed to read window state', error))
  void appWindow.onResized(() => {
    void syncMaximizedState().catch((error) => console.error('Failed to read window state', error))
  }).then((unlisten) => { unlistenResize = unlisten })
})

onBeforeUnmount(() => {
  unlistenResize?.()
  document.documentElement.classList.remove('window-maximized')
})
</script>

<template>
  <header class="app-titlebar" data-tauri-drag-region>
    <div class="app-titlebar-label" data-tauri-drag-region @dblclick="runWindowAction(() => appWindow.toggleMaximize())">
      <span class="app-titlebar-mark" aria-hidden="true"></span>
      <span data-tauri-drag-region>对话归档</span>
    </div>
    <div class="app-titlebar-controls" aria-label="窗口控制">
      <button type="button" title="最小化" aria-label="最小化" @click="runWindowAction(() => appWindow.minimize())">
        <Minus :size="15" stroke-width="1.7" />
      </button>
      <button type="button" title="最大化或还原" aria-label="最大化或还原" @click="runWindowAction(() => appWindow.toggleMaximize())">
        <Square :size="12" stroke-width="1.7" />
      </button>
      <button class="app-titlebar-close" type="button" title="关闭" aria-label="关闭" @click="runWindowAction(() => appWindow.close())">
        <X :size="15" stroke-width="1.7" />
      </button>
    </div>
  </header>
</template>
