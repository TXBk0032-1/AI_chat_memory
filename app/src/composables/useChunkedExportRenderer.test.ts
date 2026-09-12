// Pure pipeline logic: no DOM is needed, so the default node environment is
// fine — requestAnimationFrame/cancelAnimationFrame are stubbed per test to
// give the tests full control over frame scheduling.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Message, Reference } from '../conversation'
import { useChunkedExportRenderer } from './useChunkedExportRenderer'
import type { RenderedExportMessage } from './useChunkedExportRenderer'

type QueuedFrame = { id: number; callback: FrameRequestCallback; canceled: boolean }

const queue: QueuedFrame[] = []
let nextFrameId = 1
let scheduled = 0

// A controllable requestAnimationFrame queue: frames run only when a test
// flushes them, and canceled frames are skipped, exactly like a browser never
// invokes a canceled rAF handle.
function stubAnimationFrame() {
  queue.length = 0
  nextFrameId = 1
  scheduled = 0
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
    scheduled += 1
    const frame: QueuedFrame = { id: nextFrameId++, callback, canceled: false }
    queue.push(frame)
    return frame.id
  })
  vi.stubGlobal('cancelAnimationFrame', (handle: number) => {
    const frame = queue.find((item) => item.id === handle)
    if (frame) frame.canceled = true
  })
}

// Runs one animation frame: the oldest callback that is still pending.
function runFrame(): boolean {
  const index = queue.findIndex((frame) => !frame.canceled)
  if (index === -1) return false
  const [frame] = queue.splice(index, 1)
  frame.callback(0)
  return true
}

// Invokes a frame that was already canceled, simulating a browser that runs
// the callback anyway: the renderer's generation guard must keep it harmless.
function runStaleFrame(): boolean {
  const frame = queue.find((item) => item.canceled)
  if (!frame) return false
  queue.splice(queue.indexOf(frame), 1)
  frame.callback(0)
  return true
}

function flushMicrotasks() {
  return new Promise((resolve) => { setTimeout(resolve, 0) })
}

function makeMessages(count: number, prefix = 'm'): Message[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `${prefix}-${index}`,
    role: index % 2 ? 'assistant' : 'user',
    content: `content ${prefix}-${index}`,
    metadata: {},
    seq: index,
  }))
}

function fakeRender() {
  return vi.fn((value: string, _message: Message, _references: Map<number, Reference>, _query: string) => `<p>${value}</p>`)
}

describe('useChunkedExportRenderer', () => {
  beforeEach(() => {
    stubAnimationFrame()
  })

  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('renders 25 messages across exactly 4 frames of at most 8 each', () => {
    const messages = makeMessages(25)
    const references = new Map<number, Reference>()
    const render = fakeRender()
    const { rendered, restart } = useChunkedExportRenderer(() => ({ messages, references, includeThinking: false }), render)

    restart()
    expect(rendered.value).toEqual([])

    runFrame()
    expect(rendered.value).toHaveLength(8)
    runFrame()
    expect(rendered.value).toHaveLength(16)
    runFrame()
    expect(rendered.value).toHaveLength(24)
    expect(render).toHaveBeenCalledTimes(24)
    runFrame()
    expect(rendered.value).toHaveLength(25)
    expect(render).toHaveBeenCalledTimes(25)

    // 25 messages at 8 per batch need exactly 4 frames, no more.
    expect(scheduled).toBe(4)
    expect(runFrame()).toBe(false)

    expect(rendered.value.map((item) => item.message.id)).toEqual(messages.map((message) => message.id))
    expect(rendered.value[0].content).toBe('<p>content m-0</p>')
    expect(rendered.value[0].thinking).toBe('')
    expect(render).toHaveBeenNthCalledWith(1, 'content m-0', messages[0], references, '')
  })

  it('clears rendered output and drops stale frames after a second restart', () => {
    const first = makeMessages(20, 'first')
    const second = makeMessages(3, 'second')
    const references = new Map<number, Reference>()
    const render = fakeRender()
    let source = { messages: first, references, includeThinking: false }
    const { rendered, restart } = useChunkedExportRenderer(() => source, render)

    restart()
    runFrame()
    expect(rendered.value).toHaveLength(8)

    source = { messages: second, references, includeThinking: false }
    restart()
    expect(rendered.value).toEqual([])

    // The superseded generation had scheduled one more frame before the
    // restart; even if the browser ran that canceled callback, the generation
    // guard must keep it from appending anything.
    expect(runStaleFrame()).toBe(true)
    expect(rendered.value).toEqual([])

    runFrame()
    expect(rendered.value).toHaveLength(3)
    expect(rendered.value.map((item) => item.message.id)).toEqual(['second-0', 'second-1', 'second-2'])
  })

  it('keeps whenReady pending until the final frame and nextTick resolve', async () => {
    const messages = makeMessages(25)
    const { rendered, restart, whenReady } = useChunkedExportRenderer(() => ({ messages, references: new Map<number, Reference>(), includeThinking: false }), fakeRender())

    restart()
    const ready = whenReady()
    let settled = false
    void ready.then(() => { settled = true })

    runFrame()
    runFrame()
    runFrame()
    await flushMicrotasks()
    // One frame short of the end: the promise must still be pending.
    expect(settled).toBe(false)
    expect(rendered.value).toHaveLength(24)

    runFrame()
    // The last frame schedules the resolution on nextTick; nothing resolves
    // synchronously inside the frame callback itself.
    expect(settled).toBe(false)
    await ready
    expect(settled).toBe(true)
    expect(rendered.value).toHaveLength(25)
  })

  it('settles the superseded promise on cancel without committing stale results', async () => {
    const messages = makeMessages(25)
    const render = fakeRender()
    const { rendered, restart, whenReady, cancel } = useChunkedExportRenderer(() => ({ messages, references: new Map<number, Reference>(), includeThinking: false }), render)

    restart()
    runFrame()
    expect(rendered.value).toHaveLength(8)

    const superseded = whenReady()
    let settled = false
    void superseded.then(() => { settled = true })

    cancel()
    await flushMicrotasks()
    // Cancel settles the superseded generation so no caller waits forever...
    expect(settled).toBe(true)

    // ...but the pipeline is dead: a stale frame appends nothing new.
    expect(runStaleFrame()).toBe(true)
    expect(rendered.value).toHaveLength(8)
    expect(render).toHaveBeenCalledTimes(8)
  })

  it('settles the superseded promise when a restart replaces the pipeline', async () => {
    const messages = makeMessages(25)
    const { rendered, restart, whenReady } = useChunkedExportRenderer(() => ({ messages, references: new Map<number, Reference>(), includeThinking: false }), fakeRender())

    restart()
    const superseded = whenReady()
    let settled = false
    void superseded.then(() => { settled = true })

    restart()
    await flushMicrotasks()
    // A restart must not leave a caller holding the replaced generation's
    // promise waiting forever.
    expect(settled).toBe(true)
    expect(rendered.value).toEqual([])
  })

  it('does not write a completed batch into a restarted generation', () => {
    // Batch-tail race: the final frame's items are all rendered, and a
    // restart lands while the loop still holds the finished batch — after the
    // per-item guard has passed but before the batch is appended. The whole
    // old batch must be dropped, leaving the array exactly as the new
    // generation defines it.
    const first = makeMessages(8, 'first')
    const second = makeMessages(3, 'second')
    let source = { messages: first, references: new Map<number, Reference>(), includeThinking: false }
    const render = vi.fn((value: string, _message: Message, _references: Map<number, Reference>, _query: string) => {
      // Restart exactly when the last item of the final batch finishes.
      if (value === 'content first-7') {
        source = { messages: second, references: new Map<number, Reference>(), includeThinking: false }
        restart()
      }
      return `<p>${value}</p>`
    })
    const { rendered, restart } = useChunkedExportRenderer(() => source, render)

    restart()
    runFrame()
    // The restart inside the render callback already cleared the array; the
    // old generation's completed batch must not be appended after that.
    expect(rendered.value).toEqual([])

    // The new generation's frame renders exactly its own messages — the
    // final array holds no duplicates and no stale first-generation items.
    runFrame()
    expect(rendered.value.map((item) => item.message.id)).toEqual(['second-0', 'second-1', 'second-2'])
    expect(runFrame()).toBe(false)
  })

  it('degrades a throwing message to escaped plain text and keeps the pipeline alive', async () => {
    const messages = makeMessages(10)
    const render = vi.fn((value: string, message: Message, _references: Map<number, Reference>, _query: string) => {
      if (message.id === 'm-2') throw new Error('malformed message')
      return `<p>${value}</p>`
    })
    const { rendered, restart, whenReady } = useChunkedExportRenderer(() => ({ messages, references: new Map<number, Reference>(), includeThinking: false }), render)

    restart()
    const ready = whenReady()
    runFrame()
    runFrame()
    await flushMicrotasks()
    await ready
    expect(rendered.value).toHaveLength(10)
    // The poisoned message degrades to its escaped raw content...
    expect(rendered.value[2].content).toBe('<pre class="export-render-fallback">content m-2</pre>')
    // ...while every other message still renders normally.
    expect(rendered.value[1].content).toBe('<p>content m-1</p>')
    expect(rendered.value[3].content).toBe('<p>content m-3</p>')
  })

  it('degrades only the thinking render when the thinking pipeline throws', () => {
    const messages = makeMessages(1)
    messages[0].metadata = { thinking: 'thinking <text>' }
    const render = vi.fn((value: string, _message: Message, _references: Map<number, Reference>, _query: string) => {
      if (value === 'thinking <text>') throw new Error('malformed thinking')
      return `<p>${value}</p>`
    })
    const { rendered, restart } = useChunkedExportRenderer(() => ({ messages, references: new Map<number, Reference>(), includeThinking: true }), render)

    restart()
    runFrame()

    expect(rendered.value[0].content).toBe('<p>content m-0</p>')
    expect(rendered.value[0].thinking).toBe('<pre class="export-render-fallback">thinking &lt;text&gt;</pre>')
  })

  it('renders thinking only when included and the metadata value is a string', () => {
    const references = new Map<number, Reference>([[1, { cite_index: 1, url: 'https://example.com', title: 'Example', summary: '' }]])
    const messages = makeMessages(3)
    messages[0].metadata = { thinking: 'thinking text' }
    messages[1].metadata = { thinking: ['not', 'a', 'string'] }
    const render = fakeRender()
    const { rendered, restart } = useChunkedExportRenderer(() => ({ messages, references, includeThinking: true }), render)

    restart()
    runFrame()

    const items: readonly RenderedExportMessage[] = rendered.value
    expect(items).toHaveLength(3)
    expect(items[0].thinking).toBe('<p>thinking text</p>')
    expect(items[1].thinking).toBe('')
    expect(items[2].thinking).toBe('')
    expect(render).toHaveBeenCalledTimes(4)
    expect(render).toHaveBeenCalledWith(messages[0].content, messages[0], references, '')
    expect(render).toHaveBeenCalledWith('thinking text', messages[0], references, '')
  })

  it('skips thinking rendering when thinking output is excluded', () => {
    const references = new Map<number, Reference>()
    const messages = makeMessages(1)
    messages[0].metadata = { thinking: 'thinking text' }
    const render = fakeRender()
    const { rendered, restart } = useChunkedExportRenderer(() => ({ messages, references, includeThinking: false }), render)

    restart()
    runFrame()

    expect(rendered.value[0].thinking).toBe('')
    expect(render).toHaveBeenCalledTimes(1)
    expect(render).toHaveBeenCalledWith(messages[0].content, messages[0], references, '')
  })

  it('resolves immediately for an empty message list without scheduling frames', async () => {
    const { rendered, restart, whenReady } = useChunkedExportRenderer(() => ({ messages: [], references: new Map<number, Reference>(), includeThinking: false }), fakeRender())

    restart()
    expect(scheduled).toBe(0)
    await whenReady()
    expect(rendered.value).toEqual([])
  })
})
