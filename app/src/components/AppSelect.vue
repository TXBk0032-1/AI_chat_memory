<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { Check, ChevronDown } from 'lucide-vue-next'

export interface SelectOption<T = string | number | boolean> {
  value: T
  label: string
  disabled?: boolean
  icon?: any
  description?: string
}

const props = withDefaults(
  defineProps<{
    modelValue?: string | number | boolean | null
    options: SelectOption<any>[]
    placeholder?: string
    disabled?: boolean
    compact?: boolean
    block?: boolean
    ariaLabel?: string
    placement?: 'bottom' | 'top' | 'auto'
    teleport?: boolean
  }>(),
  {
    modelValue: '',
    placeholder: '',
    disabled: false,
    compact: false,
    block: false,
    ariaLabel: undefined,
    placement: 'auto',
    teleport: true,
  },
)

const emit = defineEmits<{
  'update:modelValue': [value: any]
  change: [value: any]
}>()

const isOpen = ref(false)
const highlightedIndex = ref(-1)
const triggerRef = ref<HTMLButtonElement | null>(null)
const menuRef = ref<HTMLDivElement | null>(null)
const containerRef = ref<HTMLDivElement | null>(null)

const isTopPlacement = ref(false)
const menuStyle = ref<{
  top: string
  left: string
  minWidth: string
  maxWidth: string
}>({
  top: '0px',
  left: '0px',
  minWidth: '0px',
  maxWidth: 'none',
})

const selectedOption = computed(() => {
  return props.options.find((opt) => opt.value === props.modelValue)
})

function updateMenuPosition() {
  if (!isOpen.value || !triggerRef.value) return

  const rect = triggerRef.value.getBoundingClientRect()
  const viewportHeight = window.innerHeight || document.documentElement.clientHeight
  const viewportWidth = window.innerWidth || document.documentElement.clientWidth
  const estimatedMenuHeight = Math.min(props.options.length * 36 + 12, 280)
  const gap = 5

  const spaceBelow = viewportHeight - rect.bottom
  const spaceAbove = rect.top

  let placeTop = false
  if (props.placement === 'top') {
    placeTop = true
  } else if (props.placement === 'bottom') {
    placeTop = false
  } else {
    if (spaceBelow < estimatedMenuHeight && spaceAbove >= estimatedMenuHeight) {
      placeTop = true
    } else {
      placeTop = false
    }
  }

  isTopPlacement.value = placeTop

  let top = placeTop
    ? Math.max(8, rect.top - estimatedMenuHeight - gap)
    : rect.bottom + gap

  let left = rect.left
  const width = rect.width
  const minWidth = Math.max(width, 120)

  if (left + minWidth > viewportWidth - 8) {
    left = Math.max(8, viewportWidth - minWidth - 8)
  }

  menuStyle.value = {
    top: `${Math.round(top)}px`,
    left: `${Math.round(left)}px`,
    minWidth: `${Math.round(minWidth)}px`,
    maxWidth: `${Math.max(Math.round(width), 320)}px`,
  }
}

function openMenu() {
  if (props.disabled) return
  isOpen.value = true
  const selectedIdx = props.options.findIndex((opt) => opt.value === props.modelValue)
  highlightedIndex.value = selectedIdx >= 0 ? selectedIdx : 0

  nextTick(() => {
    updateMenuPosition()
    scrollHighlightedIntoView()
  })
}

function closeMenu() {
  if (!isOpen.value) return
  isOpen.value = false
  highlightedIndex.value = -1
}

function toggleOpen() {
  if (isOpen.value) {
    closeMenu()
  } else {
    openMenu()
  }
}

function selectOption(option: SelectOption<any>) {
  if (option.disabled) return
  emit('update:modelValue', option.value)
  emit('change', option.value)
  closeMenu()
  triggerRef.value?.focus()
}

function scrollHighlightedIntoView() {
  if (!menuRef.value || highlightedIndex.value < 0) return
  const optionElements = menuRef.value.querySelectorAll<HTMLElement>('.app-select__option')
  const target = optionElements[highlightedIndex.value]
  if (target) {
    target.scrollIntoView({ block: 'nearest' })
  }
}

function handlePointerDownOutside(event: MouseEvent | TouchEvent) {
  if (!isOpen.value) return
  const target = event.target as Node | null
  if (!target) return

  if (containerRef.value?.contains(target) || menuRef.value?.contains(target)) {
    return
  }
  closeMenu()
}

function onTriggerKeydown(event: KeyboardEvent) {
  if (props.disabled) return

  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    event.preventDefault()
    if (!isOpen.value) {
      openMenu()
    } else {
      const step = event.key === 'ArrowDown' ? 1 : -1
      moveHighlight(step)
    }
  } else if (event.key === 'Enter' || event.key === ' ') {
    event.preventDefault()
    if (isOpen.value) {
      if (highlightedIndex.value >= 0 && highlightedIndex.value < props.options.length) {
        selectOption(props.options[highlightedIndex.value])
      }
    } else {
      openMenu()
    }
  } else if (event.key === 'Escape') {
    if (isOpen.value) {
      event.preventDefault()
      closeMenu()
    }
  } else if (event.key === 'Tab') {
    if (isOpen.value) {
      closeMenu()
    }
  }
}

function onMenuKeydown(event: KeyboardEvent) {
  if (event.key === 'ArrowDown') {
    event.preventDefault()
    moveHighlight(1)
  } else if (event.key === 'ArrowUp') {
    event.preventDefault()
    moveHighlight(-1)
  } else if (event.key === 'Enter' || event.key === ' ') {
    event.preventDefault()
    if (highlightedIndex.value >= 0 && highlightedIndex.value < props.options.length) {
      selectOption(props.options[highlightedIndex.value])
    }
  } else if (event.key === 'Escape') {
    event.preventDefault()
    closeMenu()
    triggerRef.value?.focus()
  } else if (event.key === 'Tab') {
    closeMenu()
  }
}

function moveHighlight(step: number) {
  if (!props.options.length) return
  let next = highlightedIndex.value + step
  if (next < 0) next = props.options.length - 1
  if (next >= props.options.length) next = 0

  let tries = 0
  while (props.options[next]?.disabled && tries < props.options.length) {
    next += step
    if (next < 0) next = props.options.length - 1
    if (next >= props.options.length) next = 0
    tries++
  }

  highlightedIndex.value = next
  nextTick(() => {
    scrollHighlightedIntoView()
  })
}

function handleScrollOrResize() {
  if (isOpen.value) {
    updateMenuPosition()
  }
}

watch(isOpen, (val) => {
  if (val) {
    window.addEventListener('resize', handleScrollOrResize, { passive: true })
    window.addEventListener('scroll', handleScrollOrResize, { capture: true, passive: true })
    document.addEventListener('pointerdown', handlePointerDownOutside, { capture: true })
  } else {
    window.removeEventListener('resize', handleScrollOrResize)
    window.removeEventListener('scroll', handleScrollOrResize, { capture: true })
    document.removeEventListener('pointerdown', handlePointerDownOutside, { capture: true })
  }
})

onMounted(() => {
  if (isOpen.value) {
    updateMenuPosition()
  }
})

onBeforeUnmount(() => {
  window.removeEventListener('resize', handleScrollOrResize)
  window.removeEventListener('scroll', handleScrollOrResize, { capture: true })
  document.removeEventListener('pointerdown', handlePointerDownOutside, { capture: true })
})
</script>

<template>
  <div
    ref="containerRef"
    class="app-select"
    :class="{
      'app-select--open': isOpen,
      'app-select--disabled': disabled,
      'app-select--compact': compact,
      'app-select--block': block,
    }"
  >
    <button
      ref="triggerRef"
      type="button"
      class="app-select__trigger"
      :aria-haspopup="'listbox'"
      :aria-expanded="isOpen"
      :aria-label="ariaLabel"
      :disabled="disabled"
      @click="toggleOpen"
      @keydown="onTriggerKeydown"
    >
      <span class="app-select__value">
        <component :is="selectedOption?.icon" v-if="selectedOption?.icon" class="app-select__icon" :size="14" />
        <span class="app-select__label">{{ selectedOption ? selectedOption.label : (placeholder || '') }}</span>
      </span>
      <ChevronDown class="app-select__chevron" :size="14" />
    </button>

    <Teleport to="body" :disabled="!teleport">
      <Transition name="app-select-dropdown">
        <div
          v-if="isOpen"
          ref="menuRef"
          class="app-select__menu"
          :class="{ 'app-select__menu--top': isTopPlacement }"
          :style="menuStyle"
          role="listbox"
          tabindex="-1"
          @keydown="onMenuKeydown"
        >
          <div class="app-select__options">
            <button
              v-for="(option, index) in options"
              :key="String(option.value)"
              type="button"
              class="app-select__option"
              :class="{
                'is-selected': option.value === modelValue,
                'is-highlighted': index === highlightedIndex,
                'is-disabled': option.disabled,
              }"
              role="option"
              :aria-selected="option.value === modelValue"
              :disabled="option.disabled"
              @mouseenter="highlightedIndex = index"
              @click="selectOption(option)"
            >
              <span class="app-select__option-content">
                <component :is="option.icon" v-if="option.icon" class="app-select__option-icon" :size="14" />
                <span class="app-select__option-label">{{ option.label }}</span>
              </span>
              <Check v-if="option.value === modelValue" class="app-select__check-icon" :size="14" />
            </button>
          </div>
        </div>
      </Transition>
    </Teleport>
  </div>
</template>
