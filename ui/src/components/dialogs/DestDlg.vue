<script setup lang="ts">
import { computed } from 'vue';
import { tr } from '../../i18n';
import { objs } from '../../util/format';
import { childOf, parentOf } from '../../util/paths';
import { closeDlg } from '../../state/overlay';
import { servers } from '../../state/servers';
import { destApply, destBlocked, destGo, destSource } from '../../state/panes';
import Icon from '../Icon.vue';

const props = defineProps<{ d: Record<string, any> }>();
const parent = computed(() => parentOf(props.d.src, props.d.path));
const blocked = computed(() => destBlocked(props.d));
</script>

<template>
  <div class="dlg w560">
    <div class="dlg-h"><span class="dlg-t ell">{{ tr(d.move ? 'Переместить' : 'Копировать') }} {{ d.names.length === 1 ? d.names[0] : objs(d.names.length) }}</span><div class="grow"></div><button class="ibtn" :title="tr('Закрыть')" @click="closeDlg(null)"><Icon n="x" /></button></div>
    <div class="dlg-b" style="gap:12px">
      <div class="dest-top">
        <select v-model="d.src" class="fld" style="width:140px;flex:none" @change="destSource(d)">
          <option value="local">{{ tr('Этот ПК') }}</option>
          <option v-for="s in servers" :key="s.id" :value="s.id">{{ s.code }}</option>
        </select>
        <input v-model="d.input" class="fld mono" :class="{ err: d.err }" :title="d.err || tr('Путь')" spellcheck="false" @keydown.enter.stop="destGo(d, d.input)" @blur="d.input !== d.path && destGo(d, d.input)">
      </div>
      <div class="dest-list scroll">
        <button v-if="parent" class="mi" @click="destGo(d, parent)"><Icon n="up" class="fup" />..</button>
        <button v-for="e in d.dirs" :key="e.name" class="mi" @click="destGo(d, childOf(d.src, d.path, e.name))"><Icon n="folder" class="fdir" /><span class="ell">{{ e.name }}</span></button>
        <div v-if="d.loading" class="dest-empty"><span class="spinner" style="width:14px;height:14px"></span></div>
        <div v-else-if="!d.dirs.length && !parent" class="dest-empty">{{ tr('Папок нет') }}</div>
      </div>
    </div>
    <div class="dlg-f"><span class="faint ell" style="font-size:12px">{{ blocked }}</span><div class="grow"></div><button class="btn ghost" @click="closeDlg(null)">{{ tr('Отмена') }}</button><button class="btn pri" :disabled="!!blocked || d.loading" @click="destApply(d)">{{ tr(d.move ? 'Переместить сюда' : 'Копировать сюда') }}</button></div>
  </div>
</template>
