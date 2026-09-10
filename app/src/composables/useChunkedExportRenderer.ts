import { nextTick, readonly, ref } from 'vue'
import type { Message, Reference } from '../conversation'
import { renderMarkdown } from '../markdown'

export interface RenderedExportMessage {
  message: Message
  content: string
  thinking: string
}

// How many messages one animation frame may render synchronously. A 25-message
// export then finishes in 4 frames, keeping each frame's markdown work short
// enough not to starve the UI thread.
export const EXPORT_RENDER_BATCH_SIZE = 8

export function useChunkedExportRenderer(
  source: () => { messages: Message[]; references: Map<number, Reference>; includeThinking: boolean },
  render: typeof renderMarkdown = renderMarkdown,
) {
  const rendered = ref<RenderedExportMessage[]>([])
  // Every restart/cancel bumps the generation; frame callbacks capture their
  // own generation and become no-ops the moment it no longer matches, so stale
  // frames can never append to a newer pipeline's output.
  let generation = 0
  let frameHandle: number | null = null
  let ready: Promise<void> = Promise.resolve()
  // Resolves the current generation's ready promise; kept next to `ready` so
  // a later generation can settle a superseded one (callers must not hang).
  let readyResolve: (() => void) | null = null

  function settleSuperseded() {
    readyResolve?.()
    readyResolve = null
  }

  function stopScheduledFrame() {
    if (frameHandle !== null) {
      cancelAnimationFrame(frameHandle)
      frameHandle = null
    }
  }

  function restart() {
    // Supersede any in-flight pipeline, then settle its ready promise so a
    // caller holding it cannot wait forever.
    generation += 1
    stopScheduledFrame()
    settleSuperseded()

    rendered.value = []
    ready = new Promise<void>((resolve) => { readyResolve = resolve })

    const currentGeneration = generation
    const { messages, references, includeThinking } = source()
    let index = 0

    function run() {
      frameHandle = null
      // A restart or cancel may have landed between scheduling and running.
      if (currentGeneration !== generation) return
      const batch = messages.slice(index, index + EXPORT_RENDER_BATCH_SIZE)
      const items: RenderedExportMessage[] = []
      for (const message of batch) {
        if (currentGeneration !== generation) return
        items.push({
          message,
          content: render(message.content, message, references, ''),
          thinking: includeThinking && typeof message.metadata?.thinking === 'string'
            ? render(message.metadata.thinking, message, references, '')
            : '',
        })
      }
      index += batch.length
      if (items.length) rendered.value.push(...items)
      if (index >= messages.length) {
        // Resolve only after the final batch has been flushed through the DOM
        // by a nextTick, so callers awaiting `whenReady()` see the whole
        // document.
        void nextTick().then(() => {
          if (currentGeneration === generation) settleSuperseded()
        })
        return
      }
      frameHandle = requestAnimationFrame(run)
    }

    if (!messages.length) {
      void nextTick().then(() => {
        if (currentGeneration === generation) settleSuperseded()
      })
      return
    }
    frameHandle = requestAnimationFrame(run)
  }

  function cancel() {
    generation += 1
    stopScheduledFrame()
    // Settle the superseded generation's promise without committing its
    // remaining results, then leave `ready` trivially settled: nothing is
    // in flight anymore.
    settleSuperseded()
    ready = Promise.resolve()
  }

  function whenReady() {
    return ready
  }

  return { rendered: readonly(rendered), restart, whenReady, cancel }
}
