import { createApp } from 'vue'
import App from './App.vue'
import { desktopApi } from './desktop-api'
import { i18n } from './i18n'
import { startDesktopApp } from './desktop-startup'

void startDesktopApp(desktopApi, (initialSettings) => {
  createApp(App, { initialSettings }).use(i18n).mount('#app')
})
