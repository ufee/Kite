<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { tr } from '../i18n';
import { isLocal } from '../util/paths';
import { ui } from '../state/prefs';
import { label, srvState } from '../state/servers';
import { panes } from '../state/panes';
import { attach, closeTerm, curTerm, fitTerm, newTermHere, selectTerm, shellName, terms, termTitle } from '../state/terms';
import { setView, startTermDrag, termHeight } from '../state/layout';
import Icon from './Icon.vue';

const host = ref<HTMLElement | null>(null);
const focusPane = computed(() => panes[ui.focus]);
const newTitle = computed(() => tr('Новый терминал') + ': ' + (isLocal(focusPane.value.src) ? shellName() : `${focusPane.value.user}@${label(focusPane.value.src)}`));

// The tab's xterm element moves into the host; switching tabs keeps every instance alive.
watch(() => curTerm.value?.id, () => nextTick(() => {
  if (host.value) attach(host.value, curTerm.value);
  tabsEl.value?.querySelector('.ttab.on')?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
}));
const tabsEl = ref<HTMLElement | null>(null);
/** A vertical wheel scrolls the tab strip sideways. */
function wheelTabs(e: WheelEvent) {
  const el = tabsEl.value;
  if (!el || el.scrollWidth <= el.clientWidth) return;
  el.scrollLeft += Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
  e.preventDefault();
}
let ro: ResizeObserver | null = null, fitTimer = 0;
onMounted(() => {
  if (curTerm.value) attach(host.value!, curTerm.value);
  // The panel animates its height: fit once it settles, not on every frame.
  ro = new ResizeObserver(() => { clearTimeout(fitTimer); fitTimer = window.setTimeout(() => fitTerm(), 60); });
  ro.observe(host.value!);
});
onBeforeUnmount(() => ro?.disconnect());

const tabTip = (t: (typeof terms)[number]) => (isLocal(t.src) ? `${termTitle(t)} · ${t.cwd}` : `${t.user}@${label(t.src)}:${t.cwd}`);
</script>

<template>
  <div class="tpanel" :class="{ split: ui.view === 'both' }" :style="{ height: termHeight + 'px' }" :aria-hidden="ui.view === 'files'">
    <div v-if="ui.view === 'both'" class="handle-h" :title="tr('Изменить высоту · двойной щелчок: только терминал')" @mousedown.prevent="startTermDrag" @dblclick="setView('term')"></div>
    <div class="thead">
      <div ref="tabsEl" class="ttabs" @wheel="wheelTabs">
        <div v-for="t in terms" :key="t.id" class="ttab" :class="{ on: t.id === ui.term }">
          <button class="ttab-main" :title="tabTip(t)" @click="selectTerm(t.id)" @mousedown.middle.prevent="closeTerm(t.id)">
            <Icon v-if="isLocal(t.src)" n="pc" s="xs" class="faint" />
            <span v-else class="dot" :class="t.dead ? '' : srvState(t.src)"></span>{{ termTitle(t) }}
          </button>
          <button class="ttab-x" :title="tr('Закрыть терминал')" @click="closeTerm(t.id)"><Icon n="x" s="xs" /></button>
        </div>
      </div>
      <button class="ibtn" :title="newTitle" @click="newTermHere"><Icon n="plus" /></button>
      <div class="grow"></div>
      <button class="ibtn" :title="ui.view === 'term' ? tr('Показать файлы') + ' · Ctrl 2' : tr('Только терминал') + ' · Ctrl 3'" @click="setView(ui.view === 'term' ? 'both' : 'term')"><Icon :n="ui.view === 'term' ? 'restore' : 'max'" /></button>
      <button class="ibtn" :title="tr('Скрыть терминал') + ' · Ctrl `'" @click="setView('files')"><Icon n="chev" /></button>
    </div>
    <div class="term">
      <div ref="host" class="xterm-box" :hidden="!curTerm"></div>
      <div v-if="!curTerm" class="term-empty">
        <div>{{ tr('Терминалов нет') }}</div>
        <button class="btn sec" @click="newTermHere">{{ tr('Открыть терминал') }}</button>
      </div>
    </div>
  </div>
</template>
