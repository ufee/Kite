<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue';
import { ui } from './state/prefs';
import { panes } from './state/panes';
import { onKey, onMouseDown, onMouseUp } from './state/keys';
import TitleBar from './components/TitleBar.vue';
import SideBar from './components/SideBar.vue';
import FilePane from './components/FilePane.vue';
import TermPanel from './components/TermPanel.vue';
import StatusBar from './components/StatusBar.vue';
import ContextMenu from './components/ContextMenu.vue';
import Dialogs from './components/Dialogs.vue';
import Palette from './components/Palette.vue';
import Toasts from './components/Toasts.vue';

const stack = ref<HTMLElement | null>(null);
let ro: ResizeObserver | null = null, animTimer = 0;
const onResize = () => { ui.vw = innerWidth; ui.vh = innerHeight; };
onMounted(() => {
  addEventListener('keydown', onKey);
  addEventListener('mousedown', onMouseDown);
  addEventListener('mouseup', onMouseUp);
  addEventListener('resize', onResize);
  // While the window is resized, the terminal height follows without animation.
  ro = new ResizeObserver(([en]) => {
    ui.stackH = Math.round(en.contentRect.height);
    ui.noanim = true;
    clearTimeout(animTimer);
    animTimer = window.setTimeout(() => { ui.noanim = false; }, 120);
  });
  ro.observe(stack.value!);
});
onBeforeUnmount(() => {
  removeEventListener('keydown', onKey);
  removeEventListener('mousedown', onMouseDown);
  removeEventListener('mouseup', onMouseUp);
  removeEventListener('resize', onResize);
  ro?.disconnect();
});
</script>

<template>
  <div class="app" :class="{ 'drag-side': ui.drag === 'side', 'drag-split': ui.drag === 'split', 'drag-term': ui.drag === 'term', noanim: ui.noanim }">
    <TitleBar />
    <div class="body">
      <SideBar />
      <main class="work">
        <div ref="stack" class="stack">
          <div class="panes">
            <div class="split-mid" :class="{ on: ui.drag === 'split' && ui.split === 0.5 }"></div>
            <FilePane v-for="(_, i) in panes" :key="i" :i="i" />
          </div>
          <TermPanel />
        </div>
        <StatusBar />
      </main>
    </div>
  </div>
  <ContextMenu />
  <Dialogs />
  <Palette />
  <Toasts />
</template>
