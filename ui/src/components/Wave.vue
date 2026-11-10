<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue';
import { waveSubscribe } from '../state/log';

// The meter repaints ten times a second: it writes styles directly, nothing else re-renders.
const root = ref<HTMLElement | null>(null);
let off: (() => void) | null = null;
onMounted(() => {
  const bars = [...root.value!.children] as HTMLElement[];
  off = waveSubscribe(v => v.forEach((x, k) => {
    const b = bars[k];
    if (!b) return;
    b.style.setProperty('--v', String(Math.abs(x)));
    b.classList.toggle('hot', x > 0);
    b.classList.toggle('bad', x < 0);
  }));
});
onBeforeUnmount(() => off?.());
</script>

<template>
  <span ref="root" class="wave" aria-hidden="true"><i v-for="k in 7" :key="k"></i></span>
</template>
