<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { Handle, Position, VueFlow, useVueFlow, type NodeMouseEvent } from '@vue-flow/core'
import { Bot, Focus, GitFork, Minus, Plus, UserRound } from 'lucide-vue-next'
import { branchLeaf, layoutBranchOverview, type BranchNodeData } from './branch-overview'
import type { BranchNode, BranchOverview } from './conversation'
import '@vue-flow/core/dist/style.css'
import { translate as t } from './i18n'

const props = defineProps<{ overview: BranchOverview; activeNodeId: string }>()
const emit = defineEmits<{ select: [node: BranchNode] }>()
const root = ref<HTMLElement | null>(null)
const flowId = `branch-overview-${Math.random().toString(36).slice(2)}`
const { fitView, zoomIn, zoomOut } = useVueFlow(flowId)
const graph = computed(() => layoutBranchOverview(props.overview, props.activeNodeId))
let resizeObserver: ResizeObserver | undefined
let fitTimer: number | undefined

function fitGraph(duration = 320) {
  window.clearTimeout(fitTimer)
  fitTimer = window.setTimeout(() => { void fitView({ padding: 0.16, minZoom: 0.12, maxZoom: 1, duration }) }, 20)
}

function selectNode(event: NodeMouseEvent) {
  const leaf = branchLeaf(props.overview.nodes, event.node.id)
  if (leaf) emit('select', leaf)
}

function nodeData(data: unknown) {
  return data as BranchNodeData
}

onMounted(() => {
  resizeObserver = new ResizeObserver(() => fitGraph(180))
  if (root.value) resizeObserver.observe(root.value)
  void nextTick(() => fitGraph())
})

watch(() => props.overview, () => void nextTick(() => fitGraph()), { deep: false })
onBeforeUnmount(() => {
  resizeObserver?.disconnect()
  window.clearTimeout(fitTimer)
})
</script>

<template>
  <div ref="root" class="branch-overview">
    <VueFlow
      :id="flowId"
      :nodes="graph.nodes"
      :edges="graph.edges"
      :min-zoom="0.12"
      :max-zoom="1.8"
      :fit-view-on-init="true"
      :pan-on-drag="true"
      :zoom-on-scroll="true"
      :zoom-on-pinch="true"
      :zoom-on-double-click="false"
      :nodes-draggable="false"
      :nodes-connectable="false"
      :elements-selectable="false"
      @node-click="selectNode"
    >
      <template #node-branch="{ data }">
        <Handle type="target" :position="Position.Top" />
        <article :class="['branch-card', nodeData(data).role, { current: nodeData(data).current, path: nodeData(data).path }]">
          <header>
            <strong><UserRound v-if="nodeData(data).role === 'user'" :size="12" /><Bot v-else :size="12" />{{ nodeData(data).roleLabel }}</strong>
            <span>#{{ nodeData(data).seq + 1 }}</span>
          </header>
          <p>{{ nodeData(data).preview || t('branch.emptyMessage') }}</p>
          <small v-if="nodeData(data).childCount">{{ t('branch.count', { count: nodeData(data).childCount }) }}</small>
        </article>
        <Handle type="source" :position="Position.Bottom" />
      </template>
      <template #node-root>
        <div class="branch-root" aria-hidden="true"><GitFork :size="15" /></div>
        <Handle type="source" :position="Position.Bottom" />
      </template>
    </VueFlow>
    <div class="branch-controls" :aria-label="t('branch.controls')">
      <button :title="t('branch.zoomIn')" :aria-label="t('branch.zoomIn')" @click="zoomIn({ duration: 180 })"><Plus :size="16" /></button>
      <button :title="t('branch.zoomOut')" :aria-label="t('branch.zoomOut')" @click="zoomOut({ duration: 180 })"><Minus :size="16" /></button>
      <button :title="t('branch.fit')" :aria-label="t('branch.fit')" @click="fitGraph()"><Focus :size="16" /></button>
    </div>
  </div>
</template>
