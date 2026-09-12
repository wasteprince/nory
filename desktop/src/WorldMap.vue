<script setup lang="ts">
import { ref, onMounted, onBeforeUnmount } from "vue";
import maskUrl from "../../assets/maps/world-land.bin?url";
const canvas = ref<HTMLCanvasElement>(),
  points: Array<[number, number]> = [];
let observer: ResizeObserver | undefined;
let themeObserver: MutationObserver | undefined;
function draw() {
  const el = canvas.value;
  if (!el) return;
  const { width: w, height: h } = el.getBoundingClientRect();
  const dpr = Math.min(devicePixelRatio || 1, 2);
  el.width = w * dpr;
  el.height = h * dpr;
  const ctx = el.getContext("2d");
  if (!ctx) return;
  ctx.scale(dpr, dpr);
  const mw = Math.min(w * 0.96, h * 1.5),
    mh = mw / 2,
    left = (w - mw) / 2,
    top = (h - mh) / 2;
  ctx.fillStyle = getComputedStyle(document.documentElement).getPropertyValue("--map-dot").trim() || "rgba(171,174,197,.065)";
  ctx.beginPath();
  for (const [x, y] of points) {
    ctx.moveTo(left + x * mw + 1, top + y * mh);
    ctx.arc(left + x * mw, top + y * mh, 0.75, 0, Math.PI * 2);
  }
  ctx.fill();
}
let disposed = false;
onMounted(async () => {
  try {
    const response = await fetch(maskUrl);
    if (!response.ok) return;
    const bytes = new Uint8Array(await response.arrayBuffer());
    if (disposed || bytes.length !== 3600) return;
    for (let i = 0; i < 240 * 120; i++)
      if (bytes[i >> 3]! & (1 << (i % 8)))
        points.push([
          ((i % 240) + 0.5) / 240,
          (Math.floor(i / 240) + 0.5) / 120,
        ]);
    observer = new ResizeObserver(draw);
    themeObserver = new MutationObserver(draw);
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    if (canvas.value) observer.observe(canvas.value);
    draw();
  } catch {
    /* A decorative asset must never prevent the interface from opening. */
  }
});
onBeforeUnmount(() => {
  disposed = true;
  observer?.disconnect();
  themeObserver?.disconnect();
});
</script>
<template>
  <canvas ref="canvas" class="world-map" aria-hidden="true" />
</template>
