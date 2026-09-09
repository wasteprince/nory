<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, useId, watch } from "vue";
import { Check, ChevronDown } from "@lucide/vue";

const props = defineProps<{
  modelValue: string;
  options: [string, string][];
  label: string;
  id?: string;
  disabled?: boolean;
}>();
const emit = defineEmits<{ "update:modelValue": [value: string] }>();
const uid = useId(),
  opened = ref(false),
  active = ref(0);
const trigger = ref<HTMLButtonElement>(),
  menu = ref<HTMLElement>();
const position = ref({
  left: "0px",
  top: "0px",
  width: "180px",
  maxHeight: "240px",
});
const selected = computed(() =>
  props.options.findIndex(([value]) => value === props.modelValue),
);
const caption = computed(
  () => props.options[selected.value]?.[1] ?? props.modelValue,
);

function close() {
  opened.value = false;
}
function choose(index: number) {
  const option = props.options[index];
  if (option && !props.disabled) emit("update:modelValue", option[0]);
  close();
  trigger.value?.focus({ preventScroll: true });
}
function place() {
  const rect = trigger.value?.getBoundingClientRect();
  if (!rect || rect.bottom < 0 || rect.top > innerHeight) {
    close();
    return;
  }
  const width = Math.min(Math.max(rect.width, 196), innerWidth - 24);
  const desired = Math.min(props.options.length * 38 + 12, 252);
  const below = innerHeight - rect.bottom - 16;
  const above = rect.top - 16;
  const down = below >= desired || below >= above;
  const height = Math.max(40, Math.min(desired, down ? below : above));
  position.value = {
    left: `${Math.max(12, Math.min(rect.right - width, innerWidth - width - 12))}px`,
    top: `${down ? rect.bottom + 6 : Math.max(8, rect.top - height - 6)}px`,
    width: `${width}px`,
    maxHeight: `${height}px`,
  };
}
async function show() {
  if (props.disabled) return;
  active.value = Math.max(0, selected.value);
  place();
  opened.value = true;
  await nextTick();
  reveal();
}
function reveal() {
  const list = menu.value;
  const option = list?.querySelector<HTMLElement>(
    `[data-index="${active.value}"]`,
  );
  if (!list || !option) return;
  // Never scroll the document or its main panel when opening a fixed popup.
  // scrollIntoView can do that and immediately trigger the outside-scroll close.
  if (option.offsetTop < list.scrollTop) list.scrollTop = option.offsetTop;
  else if (
    option.offsetTop + option.offsetHeight >
    list.scrollTop + list.clientHeight
  )
    list.scrollTop = option.offsetTop + option.offsetHeight - list.clientHeight;
}
async function key(event: KeyboardEvent) {
  if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
    event.preventDefault();
    if (!opened.value) {
      await show();
      return;
    }
    if (event.key === "Home") active.value = 0;
    else if (event.key === "End") active.value = props.options.length - 1;
    else
      active.value =
        (active.value +
          (event.key === "ArrowDown" ? 1 : -1) +
          props.options.length) %
        props.options.length;
    await nextTick();
    reveal();
  } else if (event.key === "Escape") {
    close();
    event.preventDefault();
    event.stopPropagation();
  } else if (event.key === "Tab") close();
  else if (opened.value && ["Enter", " "].includes(event.key)) {
    event.preventDefault();
    choose(active.value);
  }
}
function outside(event: PointerEvent) {
  if (
    !trigger.value?.contains(event.target as Node) &&
    !menu.value?.contains(event.target as Node)
  )
    close();
}
function scroll(event: Event) {
  // Follow the trigger without dismissing a just-opened popup on a delayed
  // viewport/scroll event. Scrolling inside the list leaves its placement alone.
  if (menu.value?.contains(event.target as Node)) return;
  place();
}
function cleanup() {
  document.removeEventListener("pointerdown", outside, true);
  window.removeEventListener("resize", place);
  window.removeEventListener("scroll", scroll, true);
}
watch(opened, (value) => {
  cleanup();
  if (value) {
    document.addEventListener("pointerdown", outside, true);
    window.addEventListener("resize", place);
    window.addEventListener("scroll", scroll, true);
  }
});
watch(
  () => props.disabled,
  (disabled) => {
    if (disabled) close();
  },
);
onBeforeUnmount(cleanup);
</script>

<template>
  <button
    ref="trigger"
    :id="id"
    class="select-trigger"
    type="button"
    role="combobox"
    :aria-label="label"
    aria-haspopup="listbox"
    :aria-expanded="opened"
    :aria-controls="`${uid}-list`"
    :aria-activedescendant="opened ? `${uid}-${active}` : undefined"
    :disabled="disabled"
    @click="opened ? close() : show()"
    @keydown="key"
  >
    <span>{{ caption }}</span
    ><ChevronDown :size="14" :class="{ 'rotate-180': opened }" />
  </button>
  <Teleport to="body">
    <div
      v-if="opened"
      ref="menu"
      :id="`${uid}-list`"
      role="listbox"
      :aria-label="label"
      class="select-menu"
      :style="position"
    >
      <div
        v-for="([value, text], index) in options"
        :key="value"
        :id="`${uid}-${index}`"
        role="option"
        :aria-selected="value === modelValue"
        :data-index="index"
        class="select-option"
        :class="{ highlighted: index === active }"
        @pointermove="active = index"
        @mousedown.prevent
        @click="choose(index)"
      >
        <span>{{ text }}</span
        ><Check v-if="value === modelValue" :size="14" />
      </div>
    </div>
  </Teleport>
</template>
