<script setup lang="ts">
import { getCurrentWindow } from '@tauri-apps/api/window'
import { Minus, Square, X } from 'lucide-vue-next'
import { onBeforeUnmount, onMounted } from 'vue'
import { translate as t } from '../i18n'

const appWindow = getCurrentWindow()
let unlistenResize: (() => void) | undefined
let isMounted = false

async function syncMaximizedState() {
  const maximized = await appWindow.isMaximized()
  document.documentElement.classList.toggle('window-maximized', maximized)
}

function runWindowAction(action: () => Promise<void>) {
  void action().catch((error) => console.error('Window action failed', error))
}

onMounted(() => {
  isMounted = true
  void syncMaximizedState().catch((error) => console.error('Failed to read window state', error))
  void appWindow.onResized(() => {
    void syncMaximizedState().catch((error) => console.error('Failed to read window state', error))
  }).then((unlisten) => {
    if (!isMounted) {
      unlisten()
    } else {
      unlistenResize = unlisten
    }
  })
})

onBeforeUnmount(() => {
  isMounted = false
  unlistenResize?.()
  document.documentElement.classList.remove('window-maximized')
})
</script>

<template>
  <header class="app-titlebar" data-tauri-drag-region>
    <div class="app-titlebar-label" data-tauri-drag-region @dblclick="runWindowAction(() => appWindow.toggleMaximize())">
      <span class="app-titlebar-mark" aria-hidden="true"></span>
      <span data-tauri-drag-region>{{ t('app.title') }}</span>
    </div>
    <div class="app-titlebar-controls" :aria-label="t('titlebar.controls')">
      <button type="button" :title="t('titlebar.minimize')" :aria-label="t('titlebar.minimize')" @click="runWindowAction(() => appWindow.minimize())">
        <Minus :size="15" stroke-width="1.7" />
      </button>
      <button type="button" :title="t('titlebar.maximizeRestore')" :aria-label="t('titlebar.maximizeRestore')" @click="runWindowAction(() => appWindow.toggleMaximize())">
        <Square :size="12" stroke-width="1.7" />
      </button>
      <button class="app-titlebar-close" type="button" :title="t('titlebar.close')" :aria-label="t('titlebar.close')" @click="runWindowAction(() => appWindow.close())">
        <X :size="15" stroke-width="1.7" />
      </button>
    </div>
  </header>
</template>
