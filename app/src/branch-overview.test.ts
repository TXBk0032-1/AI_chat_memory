import { describe, expect, it } from 'vitest'
import { branchConversation, branchLeaf, branchMessageSeqs, branchNodeHeight, branchPath, branchReadingIndex, filterBranchMatches, layoutBranchOverview, virtualBranchRootId } from './branch-overview'
import type { BranchNode, BranchOverview, SearchMatch } from './conversation'

function node(id: string, parent: string, children: string[], seq: number): BranchNode {
  return { message_id: `message-${id}`, node_id: id, parent_node_id: parent, children_node_ids: children, seq, role: seq % 2 ? 'assistant' : 'user', preview: id }
}

const overview: BranchOverview = {
  nodes: [node('root', '', ['left', 'right'], 0), node('left', 'root', [], 1), node('right', 'root', ['leaf'], 2), node('leaf', 'right', [], 3)],
  default_leaf_node_id: 'leaf',
}

const multiVersionOverview: BranchOverview = {
  nodes: [
    node('answer-a', 'question-a', [], 0), node('question-a', '', ['answer-a'], 1),
    node('answer-b', 'question-b', [], 2), node('question-b', '', ['answer-b'], 3),
    node('answer-c', 'question-c', [], 4), node('question-c', '', ['answer-c'], 5),
    node('answer-d', 'question-d', ['follow-up'], 6), node('question-d', '', ['answer-d'], 7),
    node('follow-answer', 'follow-up', ['rewrite-a', 'rewrite-b', 'rewrite-c'], 8), node('follow-up', 'answer-d', ['follow-answer'], 9),
    node('rewrite-a-answer', 'rewrite-a', [], 10), node('rewrite-a', 'follow-answer', ['rewrite-a-answer'], 11),
    node('rewrite-b-answer', 'rewrite-b', [], 12), node('rewrite-b', 'follow-answer', ['rewrite-b-answer'], 13),
    node('rewrite-c-answer', 'rewrite-c', ['last-question'], 14), node('rewrite-c', 'follow-answer', ['rewrite-c-answer'], 15),
    node('last-answer', 'last-question', [], 16), node('last-question', 'rewrite-c-answer', ['last-answer'], 17),
  ],
  default_leaf_node_id: 'last-answer',
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
    expect(branchConversation(overview.nodes, 'leaf').map((item) => item.node_id)).toEqual(['root', 'right', 'leaf'])
  })

  it('connects multiple initial versions through a virtual root', () => {
    const multipleRoots: BranchOverview = {
      nodes: [node('one', '', [], 0), node('two', '', [], 1), node('three', '', [], 2)],
      default_leaf_node_id: 'three',
    }
    const layout = layoutBranchOverview(multipleRoots, 'three')
    expect(layout.nodes.some((item) => item.id === virtualBranchRootId)).toBe(true)
    expect(layout.edges.filter((edge) => edge.source === virtualBranchRootId)).toHaveLength(3)
  })

  it('regresses multi-version exports without stacking sibling versions', () => {
    const seqs = branchMessageSeqs(multiVersionOverview, multiVersionOverview.default_leaf_node_id, 18)
    expect(seqs).toEqual([7, 6, 9, 8, 15, 14, 17, 16])
    expect(seqs).not.toContain(1)
    expect(seqs).not.toContain(11)
    expect(seqs).not.toContain(13)

    const layout = layoutBranchOverview(multiVersionOverview, multiVersionOverview.default_leaf_node_id)
    expect(layout.edges.filter((edge) => edge.source === virtualBranchRootId)).toHaveLength(4)
    expect(layout.edges).toHaveLength(multiVersionOverview.nodes.length)
  })

  it('filters search and reading restoration to the selected version path', () => {
    const seqs = branchMessageSeqs(multiVersionOverview, 'last-answer', 18)
    const matches: SearchMatch[] = [
      { message_id: 'visible', seq: 14, field: 'content', count: 1, occurrence: 0 },
      { message_id: 'hidden-root-version', seq: 1, field: 'content', count: 1, occurrence: 0 },
      { message_id: 'hidden-rewrite', seq: 12, field: 'thinking', count: 1, occurrence: 0 },
    ]
    expect(filterBranchMatches(matches, seqs).map((match) => match.message_id)).toEqual(['visible'])
    expect(branchReadingIndex(seqs, 14, 0)).toBe(5)
    expect(branchReadingIndex(seqs, 12, 0)).toBe(0)
    expect(branchReadingIndex(seqs, null, 9)).toBe(2)
  })

  it('keeps linear conversations unchanged', () => {
    expect(branchMessageSeqs(null, '', 5)).toEqual([0, 1, 2, 3, 4])
  })

  it('stops safely on malformed parent and child cycles', () => {
    const cyclic = [node('a', 'b', ['b'], 0), node('b', 'a', ['a'], 1)]
    expect(branchConversation(cyclic, 'a').map((item) => item.node_id)).toEqual(['b', 'a'])
    expect(branchLeaf(cyclic, 'a')?.node_id).toBe('a')
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
