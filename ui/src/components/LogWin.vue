<script setup lang="ts">
import { nextTick, onMounted, ref, watch } from 'vue';
import { tr } from '../i18n';
import { ui } from '../state/prefs';
import { clearLog, copyLog, log, logLine, logView, toggleLog, toggleLogErr } from '../state/log';
import { WIN_EDGES, winDrag, winRaise, winResize, winStyle } from '../state/layout';
import Icon from './Icon.vue';

// Follows new rows while scrolled to the bottom; reading older rows stops it.
const box = ref<HTMLElement | null>(null);
let stick = true;
const follow = () => nextTick(() => { const el = box.value; if (el && stick) el.scrollTop = el.scrollHeight; });
const onScroll = () => { const el = box.value!; stick = el.scrollHeight - el.scrollTop - el.clientHeight < 24; };
watch(() => [log.length, log[log.length - 1]?.state], follow);
watch(() => ui.logOnlyErr, () => { stick = true; follow(); });
onMounted(() => { stick = true; follow(); });
</script>

<template>
  <div class="pop" :style="winStyle('log')" @pointerdown.capture="winRaise('log')">
    <div class="pop-h" @pointerdown="winDrag('log', $event)">
      <span style="font-weight:600">{{ tr('Отладка') }}</span>
      <div class="grow"></div>
      <button class="ibtn log-filter" :class="{ on: ui.logOnlyErr }" :title="tr('Только ошибки')" @click="toggleLogErr"><Icon n="warn" s="s" /></button>
      <button class="ibtn" :title="tr('Копировать журнал')" @click="copyLog"><Icon n="copy" s="s" /></button>
      <button class="ibtn" :title="tr('Очистить')" @click="clearLog"><Icon n="trash" s="s" /></button>
      <button class="ibtn" :title="tr('Закрыть')" @click="toggleLog"><Icon n="x" s="s" /></button>
    </div>
    <div ref="box" class="scroll log" @scroll.passive="onScroll">
      <div v-for="r in logView" :key="r.id" class="log-row" :class="r.state" :title="logLine(r)">
        <span class="log-t">{{ r.time.slice(0, 8) }}</span>
        <span class="log-src ell">{{ r.src }}</span>
        <span class="log-tag">{{ r.tag }}</span>
        <span class="log-m"><span class="log-x ell">{{ r.text }}</span><span v-if="r.note || r.state === 'run'" class="log-n ell">{{ r.state === 'run' ? '…' : '→ ' + r.note }}</span></span>
      </div>
      <div v-if="!logView.length" class="empty-note">{{ tr(ui.logOnlyErr ? 'Ошибок нет' : 'Журнал пуст') }}</div>
    </div>
    <i v-for="d in WIN_EDGES" :key="d" class="rz" :class="d" @pointerdown="winResize('log', d, $event)"></i>
  </div>
</template>
