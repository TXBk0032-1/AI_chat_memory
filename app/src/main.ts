import { createApp } from 'vue'
import App from './App.vue'
import { desktopApi } from './desktop-api'
import { i18n, initializeLocaleAndMount } from './i18n'

export async function startDesktopApp() {
  await initializeLocaleAndMount(desktopApi, (initialSettings) => {
    createApp(App, { initialSettings }).use(i18n).mount('#app')
  })
}

void startDesktopApp()
