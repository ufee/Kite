<script setup lang="ts">
import { onBeforeUnmount, ref } from 'vue';
import type { ServerId } from '../backend';
import { tr } from '../i18n';
import { ui } from '../state/prefs';
import { openMenu } from '../state/overlay';
import { servers, sideGroups, srvState, srvTitle, toggleGroup } from '../state/servers';
import { other, panes, setSource } from '../state/panes';
import { openTerm } from '../state/terms';
import { sideWidth, startSideDrag, toggleSide } from '../state/layout';
import { openServer } from '../state/serverCard';
import Icon from './Icon.vue';

const paneMarks = (id: ServerId) => panes.map((p, i) => (p.src === id ? i : -1)).filter(i => i >= 0);

/** Ctrl+click opens the server in the other pane. */
function pickServer(id: ServerId, ev?: MouseEvent) {
  const i = ev && (ev.ctrlKey || ev.metaKey) ? other(ui.focus) : ui.focus;
  ui.focus = i;
  setSource(i, id);
}
function sideKey(ev: KeyboardEvent) {
  const el = ev.target as HTMLInputElement;
  if (ev.key === 'Enter') {
    const s = sideGroups.value.flatMap(g => g.items)[0];
    if (s) { pickServer(s.id); ui.search = ''; el.blur(); }
  } else if (ev.key === 'Escape') { ui.search = ''; el.blur(); }
}

/* Collapsed: the add button docks into the bottom once the servers no longer fit.
   Any content change resizes the list until it overflows, so observing the list is enough. */
let listEl: HTMLElement | null = null;
const ro = new ResizeObserver(([en]) => {
  const el = en.target as HTMLElement;
  ui.sideDocked = el.scrollHeight > el.clientHeight;
  placeThumb();
  if (!ui.sideDocked) thumb.value?.classList.remove('on');
});
function listRef(el: unknown) {
  const e = el as HTMLElement | null;
  if (!e || e === listEl) return;
  if (listEl) ro.unobserve(listEl);
  listEl = e;
  ro.observe(e);
}
onBeforeUnmount(() => ro.disconnect());

/* Overlay scrollbar. Styled directly, so scrolling does not re-render anything. */
const THUMB_HIDE_MS = 2500;
const thumb = ref<HTMLElement | null>(null);
let thumbTimer = 0, thumbHeld = false;
function placeThumb() {
  const t = thumb.value;
  if (!t || !listEl) return;
  const { scrollTop, scrollHeight, clientHeight, offsetTop } = listEl;
  const h = Math.max(24, Math.round((clientHeight * clientHeight) / scrollHeight));
  const y = offsetTop + Math.round(((clientHeight - h) * scrollTop) / Math.max(1, scrollHeight - clientHeight));
  t.style.height = h + 'px';
  t.style.transform = `translateY(${y}px)`;
}
function hideThumb() {
  const t = thumb.value;
  if (!t) return;
  if (thumbHeld || t.matches(':hover')) thumbTimer = window.setTimeout(hideThumb, THUMB_HIDE_MS);
  else t.classList.remove('on');
}
function showThumb() {
  placeThumb();
  thumb.value?.classList.add('on');
  clearTimeout(thumbTimer);
  thumbTimer = window.setTimeout(hideThumb, THUMB_HIDE_MS);
}
function startThumbDrag(e: MouseEvent) {
  const t = thumb.value!, l = listEl!;
  const startY = e.clientY, startTop = l.scrollTop;
  const ratio = (l.scrollHeight - l.clientHeight) / Math.max(1, l.clientHeight - t.offsetHeight);
  const move = (ev: MouseEvent) => { l.scrollTop = startTop + (ev.clientY - startY) * ratio; };
  thumbHeld = true;
  t.classList.add('held');
  addEventListener('mousemove', move);
  addEventListener('mouseup', () => {
    removeEventListener('mousemove', move);
    thumbHeld = false;
    t.classList.remove('held');
    showThumb();
  }, { once: true });
}
</script>

<template>
  <aside class="side" :class="{ docked: ui.sideDocked }" :style="{ width: sideWidth + 'px' }">
    <template v-if="!ui.collapsed">
      <div class="side-top">
        <label class="search" :title="tr('Поиск серверов') + ' · Ctrl P'"><Icon n="search" s="s" /><input v-model="ui.search" :placeholder="tr('Сервер')" spellcheck="false" @keydown="sideKey"></label>
        <button class="ibtn" :title="tr('Добавить сервер')" @click="openServer(null)"><Icon n="plus" /></button>
        <button class="ibtn" :title="tr('Сортировка и группы')" @click="openMenu('sort', $event, {}, 'right')"><Icon n="sort" /></button>
        <button class="ibtn" :title="tr('Скрыть серверы') + ' · Ctrl B'" @click="toggleSide"><Icon n="panel" /></button>
      </div>
      <div v-if="!servers.length && !ui.search" class="side-empty">
        <Icon n="server" class="side-empty-ico" />
        <div class="side-empty-t">{{ tr('Серверов пока нет') }}</div>
        <div class="faint">{{ tr('Добавь первый, чтобы подключиться') }}</div>
        <button class="btn pri" @click="openServer(null)"><Icon n="plus" s="s" />{{ tr('Добавить сервер') }}</button>
      </div>
      <div v-else :ref="listRef" class="side-list scroll" @scroll.passive="showThumb">
        <template v-for="g in sideGroups" :key="g.name">
          <button class="grp" :class="{ closed: g.closed }" :title="tr(g.closed ? 'Развернуть группу' : 'Свернуть группу')" @click="toggleGroup(g.name)">
            <Icon n="chev" s="xs" class="chev" /><span class="grp-name">{{ g.name }}</span>
            <span class="grp-count"><span v-if="g.closed && g.live" class="live" :title="tr('Подключено: {n}', { n: g.live })"></span>{{ ui.search ? g.items.length : g.total }}</span>
          </button>
          <div class="grp-body" :class="{ closed: g.closed }"><div>
            <div v-for="s in g.items" :key="s.id" class="srv-wrap">
              <button class="srv" :class="{ on: panes[ui.focus].src === s.id, open: paneMarks(s.id).length }" :title="srvTitle(s) + '\n' + tr('Ctrl+щелчок: в другой панели')"
                      :tabindex="g.closed ? -1 : 0" @click="pickServer(s.id, $event)" @contextmenu.prevent="openMenu('srv', $event, { id: s.id })">
                <span class="lead"><span class="dot" :class="srvState(s.id)"></span></span>
                <span class="srv-name">{{ s.code }}</span>
                <span class="srv-marks"><Icon v-for="k in paneMarks(s.id)" :key="k" :n="k === 0 ? 'pl' : 'pr'" s="s" :class="{ on: k === ui.focus }" /></span>
              </button>
              <button class="ibtn srv-term" :title="tr('Терминал') + ' ' + s.defaultUser + '@' + s.code" :tabindex="g.closed ? -1 : 0" @click="openTerm(s.id)"><Icon n="term" s="s" /></button>
            </div>
          </div></div>
        </template>
        <div v-if="!sideGroups.length" class="empty-note">{{ tr('Ничего не найдено') }}</div>
      </div>
    </template>
    <template v-else>
      <div class="side-top" style="justify-content:center">
        <button class="ibtn" :title="tr('Сортировка')" @click="openMenu('sort', $event)"><Icon n="sort" /></button>
        <button class="ibtn" :title="tr('Показать серверы') + ' · Ctrl B'" @click="toggleSide"><Icon n="panel" /></button>
      </div>
      <div :ref="listRef" class="side-list scroll mini" @scroll.passive="showThumb">
        <template v-for="g in sideGroups" :key="g.name">
          <div class="grp-line" :title="g.name"></div>
          <button v-for="s in g.items" :key="s.id" class="srv mini" :class="{ on: panes[ui.focus].src === s.id, open: paneMarks(s.id).length }" :title="srvTitle(s)"
                  @click="pickServer(s.id, $event)" @contextmenu.prevent="openMenu('srv', $event, { id: s.id })">
            <span class="lead"><span class="dot" :class="srvState(s.id)"></span></span><span class="srv-name">{{ s.code }}</span>
          </button>
        </template>
        <div class="grp-line"></div>
      </div>
      <div class="side-add"><button class="srv mini srv-add" :title="tr('Добавить сервер')" @click="openServer(null)"><Icon n="plus" s="l" class="srv-ico" /></button></div>
    </template>
    <div ref="thumb" class="side-thumb" @mousedown.prevent="startThumbDrag"></div>
    <div class="handle-v" :title="tr('Изменить ширину · двойной щелчок: свернуть')" @mousedown.prevent="startSideDrag" @dblclick="toggleSide"></div>
  </aside>
</template>
