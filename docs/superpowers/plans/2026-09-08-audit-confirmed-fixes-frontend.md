# 前端安全、响应性与分页恢复实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 消毒 Mermaid SVG、把大导出 Markdown 渲染拆到多帧并建立 ready 屏障，以及让分页失败后重试同一页。

**架构：** 单一 `sanitizeMermaidSvg` 模块封装固定 DOMPurify 配置；新的 chunked renderer composable 负责 generation、rAF、ready promise 和卸载取消，ExportDocument 仅消费其结果；App 的所有导出路径统一先 await 文档 ready 再做 Mermaid/图片/PDF；catalog 请求显式携带候选页并仅成功提交。

**技术栈：** Vue 3、TypeScript、Vitest、DOMPurify 3.4.14、Mermaid 11、requestAnimationFrame

---

## 文件结构

- 创建 `app/src/sanitizeMermaidSvg.ts`：DOMPurify SVG 安全策略的唯一入口。
- 创建 `app/src/sanitizeMermaidSvg.test.ts`：恶意 SVG 与正常 Mermaid SVG 测试。
- 修改 `app/src/composables/useMermaidRenderer.ts` 和测试：两个 innerHTML 写入点都先消毒。
- 创建 `app/src/composables/useChunkedExportRenderer.ts` 和测试：分帧渲染、generation、ready/cancel。
- 修改 `app/src/components/ExportDocument.vue`：替换同步 computed，暴露 `whenReady`。
- 修改 `app/src/App.vue` 和 `app/src/app-export-lock.test.ts`：所有导出入口 await ready。
- 修改 `app/src/composables/useSessionCatalog.ts` 和测试：候选页仅成功提交。
- 修改 `app/package.json`、`app/package-lock.json`：直接精确依赖 `dompurify@3.4.14`。

### 任务 1：统一消毒 Mermaid SVG

**文件：**
- 创建：`app/src/sanitizeMermaidSvg.ts`
- 创建：`app/src/sanitizeMermaidSvg.test.ts`
- 修改：`app/src/composables/useMermaidRenderer.ts:1-125`
- 修改：`app/src/composables/useMermaidRenderer.test.ts`
- 修改：`app/package.json`
- 修改：`app/package-lock.json`

- [ ] **步骤 1：安装精确直接依赖**

运行：

```powershell
npm --prefix app install --save-exact dompurify@3.4.14
```

预期：`app/package.json` 的 dependencies 出现 `"dompurify": "3.4.14"`，lockfile 更新；不得出现 `^` 或 `~`。

- [ ] **步骤 2：编写恶意 SVG 失败测试**

```typescript
import { describe, expect, it } from 'vitest'
import { sanitizeMermaidSvg } from './sanitizeMermaidSvg'

describe('sanitizeMermaidSvg', () => {
  it('removes executable SVG content', () => {
    const dirty = '<svg onload="alert(1)"><script>alert(1)</script><foreignObject><div>html</div></foreignObject><a href="javascript:alert(1)">x</a><g onclick="x()"><text>safe</text></g></svg>'
    const clean = sanitizeMermaidSvg(dirty)
    expect(clean).toContain('<svg')
    expect(clean).toContain('<text>safe</text>')
    expect(clean).not.toMatch(/script|foreignObject|onload|onclick|javascript:/i)
  })

  it('keeps normal mermaid structure and styling', () => {
    const clean = sanitizeMermaidSvg('<svg viewBox="0 0 10 10"><style>.node{fill:#fff}</style><g id="n"><path d="M0 0L1 1"/><text>Node</text></g></svg>')
    expect(clean).toMatch(/viewBox="0 0 10 10"/i)
    expect(clean).toContain('<style>')
    expect(clean).toContain('<path')
    expect(clean).toContain('Node')
  })
})
```

- [ ] **步骤 3：运行测试验证失败**

```powershell
npm --prefix app test -- --run src/sanitizeMermaidSvg.test.ts
```

预期：FAIL，模块不存在。

- [ ] **步骤 4：实现共享消毒函数**

```typescript
import DOMPurify from 'dompurify'

export function sanitizeMermaidSvg(svg: string): string {
  return DOMPurify.sanitize(svg, {
    USE_PROFILES: { svg: true, svgFilters: true },
    FORBID_TAGS: ['script', 'foreignObject'],
    FORBID_ATTR: ['onload', 'onclick', 'onerror', 'onmouseover'],
  })
}
```

DOMPurify 默认 URL 协议过滤继续生效，不设置 `ALLOW_UNKNOWN_PROTOCOLS`。若正常 Mermaid fixture 证明必须保留额外安全 SVG 属性，只在测试证明后用 `ADD_ATTR` 精确添加。

- [ ] **步骤 5：两条 Mermaid 路径都调用共享函数**

在 `useMermaidRenderer.ts` 导入函数，并把两个 `element.innerHTML = svg` 改为 `element.innerHTML = sanitizeMermaidSvg(svg)`。在既有 renderer 测试中 mock 返回恶意 SVG，分别断言 app/export 元素均没有 script 和事件属性。

- [ ] **步骤 6：运行定向测试**

```powershell
npm --prefix app test -- --run src/sanitizeMermaidSvg.test.ts src/composables/useMermaidRenderer.test.ts
```

预期：全部 PASS。

- [ ] **步骤 7：Commit**

```powershell
git add app/package.json app/package-lock.json app/src/sanitizeMermaidSvg.ts app/src/sanitizeMermaidSvg.test.ts app/src/composables/useMermaidRenderer.ts app/src/composables/useMermaidRenderer.test.ts
git commit -m "fix(frontend): 消毒 Mermaid SVG 输出`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

### 任务 2：建立导出 Markdown 分帧渲染器

**文件：**
- 创建：`app/src/composables/useChunkedExportRenderer.ts`
- 创建：`app/src/composables/useChunkedExportRenderer.test.ts`
- 修改：`app/src/components/ExportDocument.vue:1-40`

- [ ] **步骤 1：编写分帧、取消和 ready 失败测试**

定义接口：

```typescript
export interface RenderedExportMessage {
  message: Message
  content: string
  thinking: string
}

export function useChunkedExportRenderer(
  source: () => { messages: Message[]; references: Map<number, Reference>; includeThinking: boolean },
  render: typeof renderMarkdown = renderMarkdown,
) { /* returns rendered, restart, whenReady, cancel */ }
```

用可控 rAF 队列测试 25 条消息在每批 8 条时需要 4 帧；第二次 restart 后旧帧不得写入；`whenReady()` 在最后帧前保持 pending，结束后 resolve；cancel 后旧 promise resolve 但不提交旧结果。

- [ ] **步骤 2：运行测试验证失败**

```powershell
npm --prefix app test -- --run src/composables/useChunkedExportRenderer.test.ts
```

预期：FAIL，模块不存在。

- [ ] **步骤 3：实现 generation + rAF 流水线**

实现固定 `EXPORT_RENDER_BATCH_SIZE = 8`。每次 `restart()`：递增 generation、取消旧 rAF、清空 rendered、创建本代 ready promise；每帧同步渲染最多 8 条，只有 generation 仍匹配才追加；结束后 `nextTick()` 再 resolve。`cancel()` 递增 generation、cancelAnimationFrame，并 resolve 被取代代次，防止调用方永久等待。

返回：

```typescript
return { rendered: readonly(rendered), restart, whenReady: () => ready, cancel }
```

- [ ] **步骤 4：接入 ExportDocument**

移除同步 `computed`。以 `watch(() => [props.messages, props.references, props.includeThinking], restart, { immediate: true, deep: false })` 启动流水线，`onBeforeUnmount(cancel)`。`defineExpose` 改为：

```typescript
defineExpose({
  getElement: () => root.value,
  whenReady,
})
```

模板保持 `v-for="item in rendered"`。

- [ ] **步骤 5：运行 composable 测试与类型检查**

```powershell
npm --prefix app test -- --run src/composables/useChunkedExportRenderer.test.ts
npm --prefix app run build
```

预期：测试 PASS，vue-tsc/Vite 构建成功。

- [ ] **步骤 6：Commit**

```powershell
git add app/src/composables/useChunkedExportRenderer.ts app/src/composables/useChunkedExportRenderer.test.ts app/src/components/ExportDocument.vue
git commit -m "perf(export): 分帧渲染大批量 Markdown`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

### 任务 3：让所有导出流程等待文档 ready

**文件：**
- 修改：`app/src/App.vue:410-510`
- 修改：`app/src/app-export-lock.test.ts`

- [ ] **步骤 1：编写 ready 屏障失败测试**

在现有 App 导出测试中 mock `ExportDocument` 暴露 deferred `whenReady`。触发 preview、PNG/JPEG 和 PDF 三种路径，分别断言 deferred resolve 前：`renderExportMermaidDiagrams`、`toPng/toJpeg`、`printToPdf` 均未调用；resolve 后才调用。

- [ ] **步骤 2：运行测试验证失败**

```powershell
npm --prefix app test -- --run src/app-export-lock.test.ts
```

预期：FAIL，当前 nextTick 后立即开始 Mermaid/PDF。

- [ ] **步骤 3：抽取单一 ready helper**

在 App.vue 添加：

```typescript
async function readyExportRoot(): Promise<HTMLElement> {
  await nextTick()
  const document = exportDocumentRef.value
  if (!document) throw new Error(t('export.documentNotReady'))
  await document.whenReady()
  await nextTick()
  const root = document.getElement()
  if (!root) throw new Error(t('export.documentNotReady'))
  return root
}
```

`prepareExportPreview`、`renderExportImage`、PDF 分支统一调用此 helper，然后依次 Mermaid、nextTick、图片本地化、fonts.ready。删除三处重复“nextTick + getElement”。

- [ ] **步骤 4：运行导出测试与构建**

```powershell
npm --prefix app test -- --run src/app-export-lock.test.ts src/composables/useChunkedExportRenderer.test.ts
npm --prefix app run build
```

预期：全部 PASS。

- [ ] **步骤 5：Commit**

```powershell
git add app/src/App.vue app/src/app-export-lock.test.ts
git commit -m "fix(export): 导出前等待分帧文档就绪`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

### 任务 4：分页失败只重试候选页

**文件：**
- 修改：`app/src/composables/useSessionCatalog.ts:25-100`
- 修改：`app/src/composables/useSessionCatalog.test.ts`

- [ ] **步骤 1：编写失败后重试同页测试**

```typescript
it('retries the same next page after loadMore fails', async () => {
  const first = Array.from({ length: 100 }, (_, i) => ({ id: `p1-${i}` }))
  const second = [{ id: 'p2-0' }]
  const searchSessions = vi.fn()
    .mockResolvedValueOnce({ sessions: first, total: 101, search_mode: 'hybrid', semantic_status: 'ready' })
    .mockRejectedValueOnce(new Error('network'))
    .mockResolvedValueOnce({ sessions: second, total: 101, search_mode: 'hybrid', semantic_status: 'ready' })
  const catalog = useSessionCatalog(fakeApi(searchSessions))
  await catalog.loadSessions()
  await catalog.loadMore()
  await catalog.loadMore()
  expect(searchSessions.mock.calls.map(([query]) => query.offset)).toEqual([0, 100, 100])
  expect(catalog.sessions.value.at(-1)?.id).toBe('p2-0')
})
```

- [ ] **步骤 2：运行测试验证失败**

```powershell
npm --prefix app test -- --run src/composables/useSessionCatalog.test.ts
```

预期：FAIL，offset 为 `[0, 100, 200]`。

- [ ] **步骤 3：让 loadSessions 返回成功状态并显式接收页码**

签名改为 `async function loadSessions(reset = true, requestedPage = reset ? 0 : page.value): Promise<boolean>`。请求 offset 使用 `requestedPage * PAGE_SIZE`；只有 generation 匹配且请求成功时，在非 reset 分支提交 `page.value = requestedPage` 并返回 true。catch/陈旧响应返回 false。reset 成功提交 page 0。

`loadMore`：

```typescript
async function loadMore() {
  if (loading.value || sessions.value.length >= total.value) return
  const candidatePage = page.value + 1
  await loadSessions(false, candidatePage)
}
```

- [ ] **步骤 4：运行 catalog 测试**

```powershell
npm --prefix app test -- --run src/composables/useSessionCatalog.test.ts
```

预期：全部 PASS，包括既有 reset 可见集测试。

- [ ] **步骤 5：Commit**

```powershell
git add app/src/composables/useSessionCatalog.ts app/src/composables/useSessionCatalog.test.ts
git commit -m "fix(frontend): 分页成功后再提交页码`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

### 任务 5：批次 4 验证与审查循环

**文件：**
- 审查：本计划全部前端、测试和依赖文件

- [ ] **步骤 1：运行批次回归**

```powershell
npm --prefix app test -- --run src/sanitizeMermaidSvg.test.ts src/composables/useMermaidRenderer.test.ts src/composables/useChunkedExportRenderer.test.ts src/app-export-lock.test.ts src/composables/useSessionCatalog.test.ts
npm --prefix app run build
npm --prefix app test
```

预期：全部退出 0。

- [ ] **步骤 2：执行高强度审查**

重点检查：DOMPurify 配置是否保留正常 Mermaid；所有 innerHTML 路径是否消毒；旧 rAF 是否可写入；ready 是否在 DOM/Mermaid 前过早解决；所有导出入口是否等待；分页陈旧响应是否提交页码。

- [ ] **步骤 3：验证并修复发现**

对 CONFIRMED 项先补红测再修复，重跑步骤 1 并重复审查至无确认遗留项。

- [ ] **步骤 4：提交审查修复（如有）**

```powershell
git add <逐条列出本批审查修复文件>
git commit -m "fix(frontend): 收敛导出与分页审查问题`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```
