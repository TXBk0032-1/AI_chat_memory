import { ref } from 'vue'

export interface ToastNotice {
  id: number
  message: string
  duration: number
}

export function useToastQueue() {
  const toasts = ref<ToastNotice[]>([])
  const timers = new Map<number, ReturnType<typeof setTimeout>>()
  let nextId = 0

  function dismissToast(id: number) {
    const timer = timers.get(id)
    if (timer !== undefined) clearTimeout(timer)
    timers.delete(id)
    toasts.value = toasts.value.filter((toast) => toast.id !== id)
  }

  function showToast(message: string, duration = 4200) {
    const notice: ToastNotice = {
      id: ++nextId,
      message,
      duration: Math.max(1, duration),
    }
    toasts.value = [...toasts.value, notice]
    timers.set(notice.id, setTimeout(() => dismissToast(notice.id), notice.duration))
    return notice.id
  }

  function disposeToasts() {
    for (const timer of timers.values()) clearTimeout(timer)
    timers.clear()
    toasts.value = []
  }

  return { toasts, showToast, dismissToast, disposeToasts }
}
