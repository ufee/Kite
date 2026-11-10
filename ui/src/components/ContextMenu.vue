<script setup lang="ts">
import { computed } from 'vue';
import type { Entry } from '../backend';
import { tr } from '../i18n';
import { copyText } from '../util/format';
import { isLocal } from '../util/paths';
import { ui } from '../state/prefs';
import { act, menu, toast } from '../state/overlay';
import { collapseAllGroups, disconnect, recent, srvBy, srvState } from '../state/servers';
import {
  COLS, copyEntryPath, isText, newFolder, openChmod, openFile, openRow, panes, pickDest, refresh, removeSel, rowByName, setSource, startRename,
} from '../state/panes';
import { openTerm, termHere } from '../state/terms';
import { openPalette } from '../state/palette';
import { deleteServer, openServer } from '../state/serverCard';
import Icon from './Icon.vue';

const d = computed(() => menu.data);
const pane = computed(() => (typeof d.value.i === 'number' ? panes[d.value.i] : null));
const srv = computed(() => srvBy(pane.value?.src));
const menuSrv = computed(() => srvBy(d.value.id));
const entry = (): Entry | undefined => rowByName(d.value.i, d.value.name);
function copyHost() {
  const s = menuSrv.value;
  if (!s) return;
  copyText(s.host);
  toast('Адрес скопирован', 'ok', null, 1800);
}
</script>

<template>
  <div v-if="menu.key" class="menu" :style="{ left: menu.x + 'px', top: menu.y + 'px', visibility: menu.ready ? 'visible' : 'hidden' }" @contextmenu.prevent>
    <template v-if="menu.key === 'src' && pane">
      <button class="mi" @click="act(() => setSource(d.i, 'local'))"><span class="mi-check"><Icon v-if="isLocal(pane.src)" n="check" s="s" /></span><Icon n="pc" s="s" class="faint" />{{ tr('Этот ПК') }}</button>
      <div class="sep"></div>
      <div class="mi-h">{{ tr('Недавние') }}</div>
      <button v-for="id in recent.filter(id => srvBy(id))" :key="id" class="mi" @click="act(() => setSource(d.i, id))">
        <span class="mi-check"><Icon v-if="pane.src === id" n="check" s="s" /></span><span class="lead"><span class="dot" :class="srvState(id)"></span></span>{{ srvBy(id)!.code }}<span class="mi-note">{{ srvBy(id)!.description }}</span>
      </button>
      <div class="sep"></div>
      <button class="mi" @click="act(() => openPalette(d.i))"><span class="mi-check"></span>{{ tr('Все серверы…') }}<span class="mi-note">Ctrl K</span></button>
    </template>

    <template v-else-if="menu.key === 'user' && pane && srv">
      <div class="mi-h">{{ tr('Войти как') }}</div>
      <button v-for="u in srv.users" :key="u.name" class="mi" @click="act(() => setSource(d.i, pane!.src, u.name))">
        <span class="mi-check"><Icon v-if="pane.user === u.name" n="check" s="s" /></span>{{ u.name }}
        <span class="mi-note">{{ u.key === 'auto' ? tr('ключ: авто') : u.key }}{{ srv.defaultUser === u.name ? ' · ' + tr('по умолчанию') : '' }}</span>
      </button>
      <div class="sep"></div>
      <button class="mi" @click="act(() => openServer(srv!.id, true))"><span class="mi-check"><Icon n="plus" s="s" /></span>{{ tr('Добавить пользователя…') }}</button>
      <button class="mi" @click="act(() => openServer(srv!.id))"><span class="mi-check"></span>{{ tr('Изменить сервер…') }}</button>
    </template>

    <template v-else-if="menu.key === 'cols' && pane">
      <div class="mi-h">{{ tr('Колонки') }}</div>
      <button v-for="c in (isLocal(pane.src) ? COLS.slice(0, 2) : COLS)" :key="c.k" class="mi" @click="ui.cols[c.k] = !ui.cols[c.k]">
        <span class="mi-check"><Icon v-if="ui.cols[c.k]" n="check" s="s" /></span>{{ tr(c.t) }}
      </button>
      <div class="sep"></div>
      <button class="mi" @click="ui.hidden = !ui.hidden"><span class="mi-check"><Icon v-if="ui.hidden" n="check" s="s" /></span>{{ tr('Скрытые файлы') }}<span class="mi-note">Ctrl H</span></button>
    </template>

    <template v-else-if="menu.key === 'row' && pane">
      <button v-if="d.dir" class="mi" @click="act(() => openRow(d.i, entry()))"><span class="mi-check"></span>{{ tr('Открыть') }}<span class="mi-note">Enter</span></button>
      <template v-else>
        <button class="mi" :disabled="!isText(d.name)" @click="act(() => openFile(d.i, entry(), 'view'))"><span class="mi-check"></span>{{ tr('Просмотр') }}<span class="mi-note">F3</span></button>
        <button class="mi" :disabled="!isText(d.name)" @click="act(() => openFile(d.i, entry(), 'edit'))"><span class="mi-check"></span>{{ tr('Редактировать') }}<span class="mi-note">F4</span></button>
      </template>
      <button class="mi" @click="act(() => pickDest(d.i))"><span class="mi-check"></span>{{ tr('Копировать в…') }}</button>
      <button class="mi" @click="act(() => pickDest(d.i, true))"><span class="mi-check"></span>{{ tr('Переместить в…') }}</button>
      <div class="sep"></div>
      <button class="mi" @click="act(() => startRename(d.i))"><span class="mi-check"></span>{{ tr('Переименовать') }}<span class="mi-note">F2</span></button>
      <button v-if="!isLocal(pane.src)" class="mi" @click="act(() => openChmod(d.i))"><span class="mi-check"></span>{{ tr('Права и владелец…') }}</button>
      <button class="mi" @click="act(() => copyEntryPath(d.i, d.name))"><span class="mi-check"></span>{{ tr('Копировать путь') }}</button>
      <button v-if="d.dir" class="mi" @click="act(() => termHere(d.i, d.name))"><span class="mi-check"></span>{{ tr('Терминал здесь') }}</button>
      <div class="sep"></div>
      <button class="mi danger" @click="act(() => removeSel(d.i))"><span class="mi-check"></span>{{ tr('Удалить') }}<span class="mi-note">Del</span></button>
    </template>

    <template v-else-if="menu.key === 'blank'">
      <button class="mi" @click="act(() => newFolder(d.i))"><span class="mi-check"></span>{{ tr('Новая папка') }}<span class="mi-note">F7</span></button>
      <button class="mi" @click="act(() => refresh(d.i))"><span class="mi-check"></span>{{ tr('Обновить') }}<span class="mi-note">Ctrl R</span></button>
      <button class="mi" @click="act(() => termHere(d.i))"><span class="mi-check"></span>{{ tr('Терминал здесь') }}</button>
      <button class="mi" @click="act(() => copyEntryPath(d.i))"><span class="mi-check"></span>{{ tr('Копировать путь') }}</button>
    </template>

    <template v-else-if="menu.key === 'srv' && menuSrv">
      <button class="mi" @click="act(() => { ui.focus = 0; setSource(0, menuSrv!.id); })"><span class="mi-check"><Icon n="pl" s="s" class="faint" /></span>{{ tr('Открыть слева') }}</button>
      <button class="mi" @click="act(() => { ui.focus = 1; setSource(1, menuSrv!.id); })"><span class="mi-check"><Icon n="pr" s="s" class="faint" /></span>{{ tr('Открыть справа') }}</button>
      <button class="mi" @click="act(() => openTerm(menuSrv!.id))"><span class="mi-check"><Icon n="term" s="s" class="faint" /></span>{{ tr('Терминал') }}</button>
      <div class="sep"></div>
      <button class="mi" @click="act(() => openServer(menuSrv!.id))"><span class="mi-check"></span>{{ tr('Изменить…') }}</button>
      <button class="mi" @click="act(copyHost)"><span class="mi-check"></span>{{ tr('Копировать адрес') }}<span class="mi-note">{{ menuSrv.host }}</span></button>
      <button v-if="srvState(menuSrv.id) === 'live'" class="mi" @click="act(() => disconnect(menuSrv!.id))"><span class="mi-check"></span>{{ tr('Отключиться') }}</button>
      <div class="sep"></div>
      <button class="mi danger" @click="act(() => deleteServer(menuSrv!.id))"><span class="mi-check"></span>{{ tr('Удалить из списка') }}</button>
    </template>

    <template v-else-if="menu.key === 'sort'">
      <div class="mi-h">{{ tr('Сортировка') }}</div>
      <button class="mi" @click="act(() => { ui.sideSort = 'name'; })"><span class="mi-check"><Icon v-if="ui.sideSort === 'name'" n="check" s="s" /></span>{{ tr('По имени') }}</button>
      <button class="mi" @click="act(() => { ui.sideSort = 'recent'; })"><span class="mi-check"><Icon v-if="ui.sideSort === 'recent'" n="check" s="s" /></span>{{ tr('Недавние сверху') }}</button>
      <template v-if="!ui.collapsed">
        <div class="sep"></div>
        <button class="mi" @click="act(() => collapseAllGroups(true))"><span class="mi-check"></span>{{ tr('Свернуть группы') }}</button>
        <button class="mi" @click="act(() => collapseAllGroups(false))"><span class="mi-check"></span>{{ tr('Развернуть группы') }}</button>
      </template>
    </template>
  </div>
</template>
