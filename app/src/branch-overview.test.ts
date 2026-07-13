import { describe, expect, it } from 'vitest'
import { branchLeaf, branchNodeHeight, branchPath, layoutBranchOverview } from './branch-overview'
import type { BranchNode, BranchOverview } from './conversation'

function node(id: string, parent: string, children: string[], seq: number): BranchNode {
  return { message_id: `message-${id}`, node_id: id, parent_node_id: parent, children_node_ids: children, seq, role: seq % 2 ? 'assistant' : 'user', preview: id }
}

const overview: BranchOverview = {
  nodes: [node('root', '', ['left', 'right'], 0), node('left', 'root', [], 1), node('right', 'root', ['leaf'], 2), node('leaf', 'right', [], 3)],
  default_leaf_node_id: 'leaf',
}

describe('branch overview helpers', () => {
  it('lays out children below their parents', () => {
    const layout = layoutBranchOverview(overview, 'leaf')
    const root = layout.nodes.find((item) => item.id === 'root')!
    const leaf = layout.nodes.find((item) => item.id === 'leaf')!
    expect(leaf.position.y).toBeGreaterThan(root.position.y + branchNodeHeight)
    expect(layout.edges).toHaveLength(3)
  })

  it('returns the complete current path', () => {
    expect([...branchPath(overview.nodes, 'leaf')]).toEqual(['leaf', 'right', 'root'])
  })

  it('navigates from a middle node to its last descendant', () => {
    expect(branchLeaf(overview.nodes, 'root')?.node_id).toBe('leaf')
    expect(branchLeaf(overview.nodes, 'left')?.node_id).toBe('left')
    expect(branchLeaf([], 'missing')).toBeNull()
  })

  it('keeps a 202-node tree finite and non-overlapping', () => {
    const nodes = Array.from({ length: 202 }, (_, index) => {
      const parentIndex = index === 0 ? -1 : Math.floor((index - 1) / 3)
      const children = [index * 3 + 1, index * 3 + 2, index * 3 + 3]
        .filter((child) => child < 202)
        .map((child) => `node-${child}`)
      return node(`node-${index}`, parentIndex < 0 ? '' : `node-${parentIndex}`, children, index)
    })
    const layout = layoutBranchOverview({ nodes, default_leaf_node_id: 'node-201' }, 'node-201')
    const positions = new Set(layout.nodes.map((item) => `${item.position.x}:${item.position.y}`))
    expect(layout.nodes).toHaveLength(202)
    expect(layout.edges).toHaveLength(201)
    expect(positions.size).toBe(202)
    expect(layout.nodes.every((item) => Number.isFinite(item.position.x) && Number.isFinite(item.position.y))).toBe(true)
  })
})
