<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { tr } from '../i18n';
import { fmtDate, fmtSize } from '../util/format';
import { isLocal } from '../util/paths';
import { ui } from '../state/prefs';
import { openMenu } from '../state/overlay';
import { label, srvBy, srvTitle } from '../state/servers';
import {
  cancelEdit, clickRow, clickUp, commitEdit, copyPath, dragEnd, dragLeave, dragOver, dragStart, drop, dropHint, gridOf, goUp,
  hasUp, listApi, openRow, paneFoot, panes, paneW, pathBlur, pathKey, refresh, retry, rowMenu, rows, selSet, setSource, sortBy, vis,
} from '../state/panes';
import { termHere } from '../state/terms';
import { startSplitDrag } from '../state/layout';
import { openServer } from '../state/serverCard';
import Icon from './Icon.vue';

const props = defineProps<{ i: number }>();
const p = computed(() => panes[props.i]);
const list = computed(() => rows(props.i));
const grid = computed(() => ({ gridTemplateColumns: gridOf(props.i) }));

/* ---------- Virtual list: only the rows in view (plus a margin) are in the DOM ---------- */
const ROW_H = 30, OVERSCAN = 12;
const box = ref<HTMLElement | null>(null), pane = ref<HTMLElement | null>(null);
const scrollY = ref(0), viewH = ref(600);
// Rows above the entries: ".." and the new folder being named.
const head = computed(() => (hasUp(props.i) ? 1 : 0) + (p.value.edit?.isNew ? 1 : 0));
const range = computed(() => {
  const top = Math.max(0, Math.floor(scrollY.value / ROW_H) - head.value - OVERSCAN);
  const end = Math.min(list.value.length, Math.ceil((scrollY.value + viewH.value) / ROW_H) - head.value + OVERSCAN);
  return { top, end: Math.max(top, end) };
});
const slice = computed(() => list.value.slice(range.value.top, range.value.end));

function scrollTo(name: string | null) {
  const el = box.value;
  if (!el || !name) return;
  const k = name === '..' ? -1 : list.value.findIndex(e => e.name === name);
  if (name !== '..' && k < 0) return;
  const y = (k < 0 ? 0 : head.value + k) * ROW_H;
  if (y < el.scrollTop) el.scrollTop = y;
  else if (y + ROW_H > el.scrollTop + el.clientHeight - 8) el.scrollTop = y + ROW_H - el.clientHeight + 8;
}
const onScroll = () => { scrollY.value = box.value!.scrollTop; };

let ro: ResizeObserver | null = null;
onMounted(() => {
  listApi[props.i] = {
    scrollTo,
    scrollTop: (y?: number) => { const el = box.value!; if (y != null) { el.scrollTop = y; scrollY.value = el.scrollTop; } return el.scrollTop; },
    focus: () => box.value?.focus(),
  };
  ro = new ResizeObserver(entries => {
    for (const en of entries) {
      if (en.target === pane.value) paneW[props.i] = Math.round(en.contentRect.width);
      else viewH.value = Math.round(en.contentRect.height);
    }
  });
  ro.observe(pane.value!);
  ro.observe(box.value!);
});
onBeforeUnmount(() => { ro?.disconnect(); listApi[props.i] = {}; });

const focusSel = (ev: FocusEvent) => (ev.target as HTMLInputElement).select();
const isGhost = (name: string) => !!ui.dnd && ui.dnd.from === props.i && ui.dnd.names.includes(name);
</script>

<template>
  <section ref="pane" class="pane" :data-pane="i" :class="{ focus: ui.focus === i }" :style="{ flexGrow: i ? 1 - ui.split : ui.split }" @mousedown="ui.focus = i">
    <div v-if="i === 1" class="handle-split" :title="tr('Изменить ширину · двойной щелчок: поровну')" @mousedown.stop.prevent="startSplitDrag" @dblclick.stop="ui.split = 0.5"></div>
    <div class="pane-h">
      <button class="src" :title="isLocal(p.src) ? tr('Сменить источник') : srvTitle(srvBy(p.src))" @click="openMenu('src', $event, { i })">
        <Icon :n="isLocal(p.src) ? 'pc' : 'server'" s="s" />{{ label(p.src) }}<Icon n="chev" s="xs" class="faint" />
      </button>
      <button v-if="!isLocal(p.src)" class="chip" :title="tr('Пользователь')" @click="openMenu('user', $event, { i })">
        <Icon n="user" s="s" />{{ p.user }}<Icon n="chev" s="xs" class="faint" />
      </button>
      <div class="pathbox" :class="{ err: p.pathErr }">
        <input v-model="p.input" class="path-in" spellcheck="false" :title="p.pathErr || tr('Путь') + ' · Ctrl L'" @keydown="pathKey(i, $event)" @blur="pathBlur(i)" @focus="focusSel">
        <button class="copy" :class="{ done: ui.copied === i }" :title="tr(ui.copied === i ? 'Скопировано' : 'Копировать путь')" @click="copyPath(i)"><Icon :n="ui.copied === i ? 'check' : 'copy'" s="s" /></button>
      </div>
      <button class="ibtn" :title="tr('Обновить') + ' · Ctrl R'" @click="refresh(i)"><Icon n="refresh" :class="{ 'spin-once': p.spin }" /></button>
      <button class="ibtn" :title="tr('Терминал в этой папке')" @click="termHere(i)"><Icon n="term" /></button>
      <button class="ibtn" :title="tr('Колонки и вид')" @click="openMenu('cols', $event, { i }, 'right')"><Icon n="sliders" /></button>
    </div>
    <div class="cols" :style="grid">
      <button class="col-h" :class="{ on: p.sort.key === 'name' }" @click="sortBy(i, 'name')">{{ tr('Имя') }}<Icon v-if="p.sort.key === 'name'" n="chev" s="xs" :style="{ transform: p.sort.dir < 0 ? 'rotate(180deg)' : '' }" /></button>
      <button v-if="vis[i].size" class="col-h r" :class="{ on: p.sort.key === 'size' }" @click="sortBy(i, 'size')"><Icon v-if="p.sort.key === 'size'" n="chev" s="xs" :style="{ transform: p.sort.dir < 0 ? '' : 'rotate(180deg)' }" />{{ tr('Размер') }}</button>
      <button v-if="vis[i].date" class="col-h" :class="{ on: p.sort.key === 'date' }" @click="sortBy(i, 'date')">{{ tr('Изменён') }}<Icon v-if="p.sort.key === 'date'" n="chev" s="xs" :style="{ transform: p.sort.dir < 0 ? '' : 'rotate(180deg)' }" /></button>
      <template v-if="!isLocal(p.src)">
        <span v-if="vis[i].perm">{{ tr('Права') }}</span>
        <span v-if="vis[i].owner">{{ tr('Владелец') }}</span>
        <span v-if="vis[i].group">{{ tr('Группа') }}</span>
      </template>
    </div>
    <div ref="box" class="list scroll" tabindex="0" :class="{ drop: p.dropping }"
         @scroll.passive="onScroll" @click.self="p.sel = []" @contextmenu.self.prevent="openMenu('blank', $event, { i })"
         @dragover="dragOver(i, $event)" @dragleave="dragLeave(i, $event)" @drop.prevent="drop(i, $event)">
      <div v-if="hasUp(i)" class="row up" data-name=".." :title="tr('Наверх') + ' · Backspace'"
           :class="{ cur: p.cur === '..', 'drop-into': p.dropRow === '..' }" :style="grid"
           @click="clickUp(i)" @dblclick="goUp(i)" @contextmenu.prevent="openMenu('blank', $event, { i })">
        <div class="fname"><Icon n="up" class="fup" /><span>..</span></div>
      </div>
      <div v-if="p.edit && p.edit.isNew" class="row sel" :style="grid">
        <div class="fname"><Icon n="folder" class="fdir" />
          <input v-model="p.edit.value" class="edit-in" spellcheck="false" @keydown.enter.stop="commitEdit(i)" @keydown.esc.stop="cancelEdit(i)" @blur="commitEdit(i)">
        </div>
      </div>
      <div :style="{ height: range.top * ROW_H + 'px' }"></div>
      <div v-for="e in slice" :key="e.name" class="row" :data-name="e.name" draggable="true"
           :class="{ sel: selSet[i].value.has(e.name), cur: p.cur === e.name, 'drop-into': p.dropRow === e.name, ghost: isGhost(e.name) }"
           :style="grid"
           @click="clickRow(i, e, $event)" @dblclick="openRow(i, e)" @contextmenu.prevent="rowMenu(i, e, $event)"
           @dragstart="dragStart(i, e, $event)" @dragend="dragEnd">
        <div class="fname">
          <Icon :n="e.dir ? 'folder' : 'file'" :class="e.dir ? 'fdir' : 'ffile'" />
          <input v-if="p.edit && !p.edit.isNew && p.edit.name === e.name" v-model="p.edit.value" class="edit-in" spellcheck="false"
                 @keydown.enter.stop="commitEdit(i)" @keydown.esc.stop="cancelEdit(i)" @blur="commitEdit(i)" @click.stop @dblclick.stop>
          <span v-else :title="e.name.length > 28 ? e.name : undefined">{{ e.name }}</span>
        </div>
        <div v-if="vis[i].size" class="num">{{ e.dir ? '' : fmtSize(e.size) }}</div>
        <div v-if="vis[i].date" class="cell">{{ fmtDate(e.mtime) }}</div>
        <template v-if="!isLocal(p.src)">
          <div v-if="vis[i].perm" class="perm">{{ e.perm }}</div>
          <div v-if="vis[i].owner" class="cell" :title="e.owner">{{ e.owner }}</div>
          <div v-if="vis[i].group" class="cell" :title="e.group">{{ e.group }}</div>
        </template>
      </div>
      <div :style="{ height: (list.length - range.end) * ROW_H + 'px' }"></div>
      <div v-if="p.state === 'ok' && !list.length && !(p.edit && p.edit.isNew)" class="empty-note">{{ tr('Папка пуста') }}</div>
    </div>
    <div class="pane-foot"><span>{{ paneFoot(i) }}</span><span v-if="p.dropping || p.dropRow" style="color:var(--ac)">{{ dropHint(i) }}</span></div>

    <div v-if="p.state === 'loading'" class="pane-state"><div class="spinner"></div><div>{{ p.msg?.title }}</div></div>
    <div v-else-if="p.state === 'error'" class="pane-state">
      <Icon n="warn" style="width:22px;height:22px;color:var(--bad)" />
      <div style="font-weight:600;color:var(--tx)">{{ p.msg?.title }}</div>
      <div class="faint" style="max-width:340px;text-align:center">{{ p.msg?.text }}</div>
      <div style="display:flex;gap:8px;margin-top:6px">
        <button class="btn sec" @click="retry(i)">{{ tr('Повторить') }}</button>
        <button v-if="!isLocal(p.src)" class="btn ghost" @click="openServer(p.src)">{{ tr('Изменить сервер') }}</button>
        <button class="btn ghost" @click="setSource(i, 'local')">{{ tr('Этот ПК') }}</button>
      </div>
    </div>
    <div v-else-if="p.state === 'off'" class="pane-state">
      <Icon n="plug" style="width:22px;height:22px;color:var(--tx3)" />
      <div>{{ p.msg?.title }}</div>
      <button class="btn sec" @click="retry(i)">{{ tr('Подключиться') }}</button>
    </div>
  </section>
</template>
