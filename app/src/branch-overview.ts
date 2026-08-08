import dagre from '@dagrejs/dagre'
import { Position, type Edge, type Node } from '@vue-flow/core'
import type { BranchNode, BranchOverview, SearchMatch } from './conversation'
import { translate as t } from './i18n'

export const branchNodeWidth = 210
export const branchNodeHeight = 76
export const virtualBranchRootId = '__branch_overview_root__'

export type BranchNodeData = BranchNode & {
  roleLabel: string
  childCount: number
  current: boolean
  path: boolean
}

export type BranchRootData = { root: true }

export function branchPath(nodes: BranchNode[], leafId: string): Set<string> {
  const byId = new Map(nodes.map((node) => [node.node_id, node]))
  const path = new Set<string>()
  let current = leafId
  while (current && byId.has(current) && !path.has(current)) {
    path.add(current)
    current = byId.get(current)?.parent_node_id ?? ''
  }
  return path
}

export function branchConversation(nodes: BranchNode[], leafId: string): BranchNode[] {
  const byId = new Map(nodes.map((node) => [node.node_id, node]))
  const conversation: BranchNode[] = []
  const visited = new Set<string>()
  let current = byId.get(leafId)
  while (current && !visited.has(current.node_id)) {
    visited.add(current.node_id)
    conversation.push(current)
    current = byId.get(current.parent_node_id)
  }
  return conversation.reverse()
}

export function branchMessageSeqs(overview: BranchOverview | null, activeLeafId: string, messageCount: number): number[] {
  if (overview && activeLeafId) return branchConversation(overview.nodes, activeLeafId).map((node) => node.seq)
  return Array.from({ length: messageCount }, (_, seq) => seq)
}

export function filterBranchMatches(matches: SearchMatch[], displayedSeqs: number[]): SearchMatch[] {
  const visible = new Set(displayedSeqs)
  return matches.filter((match) => visible.has(match.seq))
}

export function branchReadingIndex(displayedSeqs: number[], savedSeq: number | null, fallbackSeq: number): number {
  const savedIndex = savedSeq === null ? -1 : displayedSeqs.indexOf(savedSeq)
  if (savedIndex >= 0) return savedIndex
  const fallbackIndex = displayedSeqs.indexOf(fallbackSeq)
  return fallbackIndex >= 0 ? fallbackIndex : 0
}

export function branchLeaf(nodes: BranchNode[], startId: string): BranchNode | null {
  const byId = new Map(nodes.map((node) => [node.node_id, node]))
  let current = byId.get(startId)
  const visited = new Set<string>()
  while (current && !visited.has(current.node_id)) {
    visited.add(current.node_id)
    const child = current.children_node_ids[current.children_node_ids.length - 1]
    if (!child || !byId.has(child)) return current
    current = byId.get(child)
  }
  return current ?? null
}

export function layoutBranchOverview(overview: BranchOverview, activeLeafId: string) {
  const graph = new dagre.graphlib.Graph()
  graph.setDefaultEdgeLabel(() => ({}))
  graph.setGraph({ rankdir: 'TB', nodesep: 34, ranksep: 62, marginx: 28, marginy: 28 })
  const roots = overview.nodes.filter((node) => !node.parent_node_id)
  const hasVirtualRoot = roots.length > 1
  if (hasVirtualRoot) graph.setNode(virtualBranchRootId, { width: 28, height: 28 })
  for (const node of overview.nodes) graph.setNode(node.node_id, { width: branchNodeWidth, height: branchNodeHeight })
  for (const node of overview.nodes) {
    if (node.parent_node_id) graph.setEdge(node.parent_node_id, node.node_id)
    else if (hasVirtualRoot) graph.setEdge(virtualBranchRootId, node.node_id)
  }
  dagre.layout(graph)

  const path = branchPath(overview.nodes, activeLeafId)
  const nodes: Array<Node<BranchNodeData | BranchRootData>> = overview.nodes.map((node) => {
    const point = graph.node(node.node_id)
    const roleLabel = node.role === 'user' ? t('app.roleYou') : node.role === 'assistant' ? t('app.roleAi') : node.role
    const preview = node.preview || t('branch.emptyMessage')
    return {
      id: node.node_id,
      type: 'branch',
      position: { x: point.x - branchNodeWidth / 2, y: point.y - branchNodeHeight / 2 },
      sourcePosition: Position.Bottom,
      targetPosition: Position.Top,
      draggable: false,
      selectable: false,
      connectable: false,
      focusable: true,
      width: branchNodeWidth,
      height: branchNodeHeight,
      ariaLabel: t('branch.nodeLabel', { role: roleLabel, preview }),
      data: {
        ...node,
        roleLabel,
        childCount: node.children_node_ids.length,
        current: node.node_id === activeLeafId,
        path: path.has(node.node_id),
      },
    }
  })
  if (hasVirtualRoot) {
    const point = graph.node(virtualBranchRootId)
    nodes.unshift({
      id: virtualBranchRootId,
      type: 'root',
      position: { x: point.x - 14, y: point.y - 14 },
      sourcePosition: Position.Bottom,
      draggable: false,
      selectable: false,
      connectable: false,
      focusable: false,
      width: 28,
      height: 28,
      data: { root: true },
    })
  }
  const edges: Edge[] = overview.nodes.flatMap((node) => node.parent_node_id ? [{
    id: `${node.parent_node_id}-${node.node_id}`,
    source: node.parent_node_id,
    target: node.node_id,
    type: 'smoothstep',
    selectable: false,
    focusable: false,
    animated: path.has(node.parent_node_id) && path.has(node.node_id),
    class: path.has(node.parent_node_id) && path.has(node.node_id) ? 'branch-edge-path' : 'branch-edge-muted',
  }] : hasVirtualRoot ? [{
    id: `${virtualBranchRootId}-${node.node_id}`,
    source: virtualBranchRootId,
    target: node.node_id,
    type: 'smoothstep',
    selectable: false,
    focusable: false,
    animated: path.has(node.node_id),
    class: path.has(node.node_id) ? 'branch-edge-path' : 'branch-edge-muted',
  }] : [])
  return { nodes, edges }
}
