import { computed, ref } from 'vue'
import type { DesktopApi } from '../desktop-api'
import type { ConversationTurn, ExportFormat } from '../conversation-export'

/**
 * 导出选择管理 Composable
 * 
 * 职责：
 * - 管理导出选择的状态（模式、格式、包含内容）
 * - 处理导出预览和图片大小检查
 * - 防护导出过程中的竞态条件
 */
export function useExportSelection(
  selectedSessionId: () => string | null,
  selectedBranchId: () => string | null,
) {
  // 选择和配置状态
  const selecting = ref(false)
  const selectLoading = ref(false)
  const exportFormat = ref<ExportFormat>('png')
  const exportIncludeThinking = ref(false)
  
  // 导出状态
  const busy = ref(false)
  const imageChecking = ref(false)
  const imageTooLarge = ref(false)
  const imageDisabledReason = ref('')
  const error = ref('')
  
  // 当前导出会话和分支的快照，用于防止竞态条件
  const lockedSessionId = ref<string | null>(null)
  const lockedBranchId = ref<string | null>(null)
  const selectedTurns = ref<ConversationTurn[]>([])

  const imageDisabled = computed(() => imageTooLarge.value || Boolean(imageDisabledReason.value))
  const canExport = computed(() => {
    const isImageExport = exportFormat.value === 'png' || exportFormat.value === 'jpeg'
    const imageOk = !isImageExport || !imageDisabled.value
    return !busy.value && !selecting.value && selectedTurns.value.length > 0 && imageOk
  })

  /**
   * 开始导出选择流程
   * 快照当前会话和分支 ID，后续所有验证都用快照比对
   */
  function startSelection() {
    if (selecting.value || busy.value) return
    selecting.value = true
    error.value = ''
    // 快照当前会话和分支，确保导出过程中用户切换不会影响
    lockedSessionId.value = selectedSessionId()
    lockedBranchId.value = selectedBranchId()
  }

  /**
   * 取消导出选择
   */
  function cancelSelection() {
    selecting.value = false
    selectLoading.value = false
    selectedTurns.value = []
    lockedSessionId.value = null
    lockedBranchId.value = null
    error.value = ''
    imageChecking.value = false
    imageTooLarge.value = false
    imageDisabledReason.value = ''
  }

  /**
   * 完成选择，准备导出
   */
  function finishSelection(turns: ConversationTurn[]) {
    if (!selecting.value || turns.length === 0) return
    selectLoading.value = false
    selectedTurns.value = turns
    selecting.value = false
  }

  /**
   * 验证导出环境，确保用户未切换会话
   * @returns 验证是否通过
   */
  function validateExportContext(): boolean {
    const currentSessionId = selectedSessionId()
    const currentBranchId = selectedBranchId()
    
    if (currentSessionId !== lockedSessionId.value || currentBranchId !== lockedBranchId.value) {
      error.value = '当前对话分支已变化，请重新选择导出内容'
      return false
    }
    return true
  }

  /**
   * 标记导出开始
   */
  function startExport() {
    if (!validateExportContext()) return false
    busy.value = true
    error.value = ''
    return true
  }

  /**
   * 完成导出
   */
  function finishExport(success: boolean, errorMessage?: string) {
    busy.value = false
    if (!success && errorMessage) {
      error.value = errorMessage
    }
  }

  /**
   * 设置图片检查状态
   */
  function checkImageSize(tooLarge: boolean, reason = '') {
    imageChecking.value = false
    imageTooLarge.value = tooLarge
    imageDisabledReason.value = reason
  }

  return {
    // 状态
    selecting,
    selectLoading,
    exportFormat,
    exportIncludeThinking,
    busy,
    imageChecking,
    imageTooLarge,
    imageDisabledReason,
    error,
    selectedTurns,
    
    // 计算属性
    imageDisabled,
    canExport,
    lockedSessionId,
    lockedBranchId,
    
    // 方法
    startSelection,
    cancelSelection,
    finishSelection,
    validateExportContext,
    startExport,
    finishExport,
    checkImageSize,
  }
}
