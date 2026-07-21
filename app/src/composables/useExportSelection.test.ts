import { describe, expect, it, beforeEach } from 'vitest'
import { useExportSelection } from './useExportSelection'
import type { ConversationTurn } from '../conversation-export'

describe('useExportSelection', () => {
  let selectedSessionId: string | null
  let selectedBranchId: string | null
  let composable: ReturnType<typeof useExportSelection>

  beforeEach(() => {
    selectedSessionId = 'session-1'
    selectedBranchId = 'branch-1'
    composable = useExportSelection(
      () => selectedSessionId,
      () => selectedBranchId,
    )
  })

  it('初始化时所有状态为空', () => {
    expect(composable.selecting.value).toBe(false)
    expect(composable.busy.value).toBe(false)
    expect(composable.selectedTurns.value).toEqual([])
    expect(composable.lockedSessionId.value).toBeNull()
  })

  it('startSelection 快照当前会话和分支', () => {
    composable.startSelection()
    expect(composable.selecting.value).toBe(true)
    expect(composable.lockedSessionId.value).toBe('session-1')
    expect(composable.lockedBranchId.value).toBe('branch-1')
  })

  it('用户切换会话时 validateExportContext 返回 false', () => {
    composable.startSelection()
    expect(composable.lockedSessionId.value).toBe('session-1')
    
    // 模拟用户切换会话
    selectedSessionId = 'session-2'
    
    const valid = composable.validateExportContext()
    expect(valid).toBe(false)
    expect(composable.error.value).toContain('当前对话分支已变化')
  })

  it('会话未变化时 validateExportContext 返回 true', () => {
    composable.startSelection()
    const valid = composable.validateExportContext()
    expect(valid).toBe(true)
  })

  it('finishSelection 设置导出转换', () => {
    const turns: ConversationTurn[] = [
      { seq: 0, role: 'user', message_id: 'msg-1', branch: [] },
      { seq: 1, role: 'assistant', message_id: 'msg-2', branch: [] },
    ]
    
    composable.startSelection()
    composable.finishSelection(turns)
    
    expect(composable.selecting.value).toBe(false)
    expect(composable.selectedTurns.value).toEqual(turns)
  })

  it('cancelSelection 清理所有导出状态', () => {
    composable.startSelection()
    composable.imageTooLarge.value = true
    composable.error.value = 'test error'
    
    composable.cancelSelection()
    
    expect(composable.selecting.value).toBe(false)
    expect(composable.busy.value).toBe(false)
    expect(composable.selectedTurns.value).toEqual([])
    expect(composable.lockedSessionId.value).toBeNull()
    expect(composable.imageTooLarge.value).toBe(false)
    expect(composable.error.value).toBe('')
  })

  it('startExport 验证上下文并设置 busy 状态', () => {
    composable.startSelection()
    const started = composable.startExport()
    
    expect(started).toBe(true)
    expect(composable.busy.value).toBe(true)
  })

  it('startExport 在会话变化时返回 false', () => {
    composable.startSelection()
    selectedSessionId = 'session-2'
    
    const started = composable.startExport()
    
    expect(started).toBe(false)
    expect(composable.busy.value).toBe(false)
  })

  it('finishExport 清理导出状态并可选设置错误', () => {
    composable.busy.value = true
    composable.finishExport(true)
    
    expect(composable.busy.value).toBe(false)
    expect(composable.error.value).toBe('')
  })

  it('finishExport 失败时设置错误信息', () => {
    composable.busy.value = true
    composable.finishExport(false, '导出失败')
    
    expect(composable.busy.value).toBe(false)
    expect(composable.error.value).toBe('导出失败')
  })

  it('checkImageSize 更新图片检查状态', () => {
    composable.imageChecking.value = true
    composable.checkImageSize(true, '图片过大')
    
    expect(composable.imageChecking.value).toBe(false)
    expect(composable.imageTooLarge.value).toBe(true)
    expect(composable.imageDisabledReason.value).toBe('图片过大')
  })

  it('imageDisabled 当图片过大或有理由时返回 true', () => {
    expect(composable.imageDisabled.value).toBe(false)
    
    composable.imageTooLarge.value = true
    expect(composable.imageDisabled.value).toBe(true)
    
    composable.imageTooLarge.value = false
    composable.imageDisabledReason.value = 'some reason'
    expect(composable.imageDisabled.value).toBe(true)
  })

  it('canExport 检查所有导出条件', () => {
    // 首先要开始选择和完成选择
    composable.startSelection()
    composable.finishSelection([
      { seq: 0, role: 'user', message_id: 'msg-1', branch: [] },
    ])
    
    // 没有 PNG/JPEG 格式限制时可以导出
    composable.exportFormat.value = 'markdown'
    expect(composable.canExport.value).toBe(true)
    
    // 忙碌时不能导出
    composable.busy.value = true
    expect(composable.canExport.value).toBe(false)
    
    // 重置并清空选择
    composable.busy.value = false
    composable.selectedTurns.value = []
    expect(composable.canExport.value).toBe(false)
  })
})
