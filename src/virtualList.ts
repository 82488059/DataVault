import { computed, ref, watch, type Ref } from "vue";

export interface VirtualRange {
  start: number;
  end: number;
  padTop: number;
  padBottom: number;
}

/** Visible slice plus spacer sizes. `rowPx` is the fixed pitch used once a list is windowed. */
export function virtualRange(
  scrollTop: number,
  viewPx: number,
  count: number,
  rowPx: number,
  overscan = 8,
): VirtualRange {
  if (count <= 0 || rowPx <= 0) {
    return { start: 0, end: 0, padTop: 0, padBottom: 0 };
  }
  const start = Math.max(0, Math.floor(scrollTop / rowPx) - overscan);
  const visible = Math.max(1, Math.ceil(viewPx / rowPx));
  const end = Math.min(count, start + visible + overscan * 2);
  return {
    start,
    end,
    padTop: start * rowPx,
    padBottom: Math.max(0, count - end) * rowPx,
  };
}

/**
 * Render every row until `threshold`, then only the rows inside the scrollport.
 * Short lists keep wrapping names. Long lists use a fixed row pitch.
 */
export function useVirtualList<T>(
  items: { readonly value: readonly T[] },
  rowPx: number,
  threshold = 200,
  resetKey?: Ref<unknown>,
) {
  const scrollTop = ref(0);
  const viewPx = ref(720);
  const scroller = ref<HTMLElement | null>(null);
  const windowed = computed(() => items.value.length > threshold);
  const range = computed(() => {
    if (!windowed.value) {
      return { start: 0, end: items.value.length, padTop: 0, padBottom: 0 };
    }
    return virtualRange(scrollTop.value, viewPx.value, items.value.length, rowPx);
  });
  const slice = computed(() => items.value.slice(range.value.start, range.value.end));

  function onScroll(ev: Event) {
    const el = ev.currentTarget as HTMLElement | null;
    if (!el) return;
    scrollTop.value = el.scrollTop;
    if (el.clientHeight > 0) viewPx.value = el.clientHeight;
  }

  function setScroller(el: unknown) {
    scroller.value = el instanceof HTMLElement ? el : null;
    if (scroller.value && scroller.value.clientHeight > 0) {
      viewPx.value = scroller.value.clientHeight;
    }
  }

  if (resetKey) {
    watch(resetKey, () => {
      scrollTop.value = 0;
      if (scroller.value) scroller.value.scrollTop = 0;
    });
  }

  return { windowed, range, slice, onScroll, setScroller };
}
