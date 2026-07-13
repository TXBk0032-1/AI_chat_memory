import dagre from '@dagrejs/dagre'
import { Position, type Edge, type Node } from '@vue-flow/core'
import type { BranchNode, BranchOverview } from './conversation'

export const branchNodeWidth = 210
export const branchNodeHeight = 76

export type BranchNodeData = BranchNode & {
  roleLabel: string
  childCount: number
  current: boolean
  path: boolean
}

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

export function branchLeaf(nodes: BranchNode[], startId: string): BranchNode | null {
  const byId = new Map(nodes.map((node) => [node.node_id, node]))
  let current = byId.get(startId)
  const visited = new Set<string>()
  while (current && visited.add(current.node_id)) {
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
  for (const node of overview.nodes) graph.setNode(node.node_id, { width: branchNodeWidth, height: branchNodeHeight })
  for (const node of overview.nodes) {
    if (node.parent_node_id) graph.setEdge(node.parent_node_id, node.node_id)
  }
  dagre.layout(graph)

  const path = branchPath(overview.nodes, activeLeafId)
  const nodes: Array<Node<BranchNodeData>> = overview.nodes.map((node) => {
    const point = graph.node(node.node_id)
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
      ariaLabel: `${node.role === 'user' ? '你' : 'AI'}：${node.preview || '空消息'}`,
      data: {
        ...node,
        roleLabel: node.role === 'user' ? '你' : node.role === 'assistant' ? 'AI' : node.role,
        childCount: node.children_node_ids.length,
        current: node.node_id === activeLeafId,
        path: path.has(node.node_id),
      },
    }
  })
  const edges: Edge[] = overview.nodes.flatMap((node) => node.parent_node_id ? [{
    id: `${node.parent_node_id}-${node.node_id}`,
    source: node.parent_node_id,
    target: node.node_id,
    type: 'smoothstep',
    selectable: false,
    focusable: false,
    animated: path.has(node.parent_node_id) && path.has(node.node_id),
    class: path.has(node.parent_node_id) && path.has(node.node_id) ? 'branch-edge-path' : 'branch-edge-muted',
  }] : [])
  return { nodes, edges }
}
