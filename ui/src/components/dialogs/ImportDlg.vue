<script setup lang="ts">
import { tr, trn } from '../../i18n';
import { closeDlg } from '../../state/overlay';
import { sideGroups } from '../../state/servers';
import {
  applyImport, copyAgent, copyExport, exportCount, exportJson, impDiff, impFile, impPaste, parseImport, SAMPLE, saveExport, statusColor,
} from '../../state/importExport';
import Icon from '../Icon.vue';

defineProps<{ d: Record<string, any> }>();
const groupCount = (list: { group: string }[]) => new Set(list.map(s => s.group)).size;
const FORMAT = `{ "version": 1,
  "groups": [
    { "name": "Production",
      "servers": [
        { "code": "web1",
          "description": "Web",
          "host": "203.0.113.17",
          "port": 22,
          "users": [
            { "name": "root", "default": true },
            { "name": "deploy",
              "key": "deploy_ed25519" } ] } ] } ] }`;
const desc = (s: Record<string, any>) => s.desc ?? s.description;
</script>

<template>
  <div class="dlg w880">
    <div class="dlg-h">
      <div class="seg"><button :class="{ on: d.tab === 'import' }" @click="d.tab = 'import'">{{ tr('Импорт') }}</button><button :class="{ on: d.tab === 'export' }" @click="d.tab = 'export'">{{ tr('Экспорт') }}</button></div>
      <div class="grow"></div><button class="ibtn" :title="tr('Закрыть') + ' · Esc'" @click="closeDlg(null)"><Icon n="x" /></button>
    </div>
    <template v-if="d.tab === 'import'">
      <div class="dlg-b" style="display:grid;grid-template-columns:340px minmax(0,1fr);gap:24px;height:470px">
        <div style="display:flex;flex-direction:column;gap:16px;min-height:0">
          <label class="drop-zone" :class="{ on: d.over }" @dragover.prevent="d.over = true" @dragleave="d.over = false" @drop.prevent="impFile($event)">
            <input type="file" accept=".json,application/json" style="display:none" @change="impFile($event)">
            <template v-if="d.fileName">
              <Icon n="json" style="width:22px;height:22px;color:var(--ac)" />
              <span style="font-weight:500">{{ d.fileName }}</span>
              <span class="faint" style="font-size:12px">{{ d.parsed ? trn(d.parsed.length, 'сервер|сервера|серверов') + ' · ' + trn(groupCount(d.parsed), 'группа|группы|групп') : tr('ошибка формата') }}</span>
            </template>
            <template v-else>
              <Icon n="json" style="width:22px;height:22px;color:var(--tx3)" />
              <span>{{ tr('Перетащите servers.json') }}<br><span class="faint">{{ tr('или выберите файл') }}</span></span>
            </template>
          </label>
          <div style="display:flex;gap:8px;margin-top:-6px">
            <button class="textbtn" style="margin-left:-10px" @click="parseImport(SAMPLE, tr('пример.json'))">{{ tr('Пример') }}</button>
            <button class="textbtn" @click="impPaste">{{ tr('Из буфера') }}</button>
          </div>
          <div style="flex:1;min-height:0;display:flex;flex-direction:column">
            <div style="display:flex;align-items:center;margin-bottom:8px"><h3 class="h3" style="margin:0">{{ tr('Формат') }}</h3><div class="grow"></div><button class="textbtn" style="margin-right:-10px" :title="tr('Задание для агента')" @click="copyAgent"><Icon n="copy" s="s" />{{ tr('Копировать для агента') }}</button></div>
            <pre class="code scroll" style="flex:1;min-height:0">{{ FORMAT }}</pre>
            <div class="faint" style="font-size:12px;margin-top:8px">{{ tr('Без паролей и ключей. Совпадения ищутся по code.') }}</div>
          </div>
        </div>
        <div style="display:flex;flex-direction:column;min-height:0">
          <div v-if="d.err" class="note bad"><Icon n="warn" /><div>{{ d.err }}</div></div>
          <template v-else-if="impDiff">
            <div style="margin-bottom:10px"><span style="color:var(--ok)">{{ tr('Новых: {n}', { n: impDiff.nNew }) }}</span> · <span style="color:var(--warn)">{{ tr('Изменено: {n}', { n: impDiff.nChanged }) }}</span> · <span class="muted">{{ tr('Без изменений: {n}', { n: impDiff.nSame }) }}</span><span v-if="impDiff.nRemoved" style="color:var(--bad)"> · {{ tr('Удалить: {n}', { n: impDiff.nRemoved }) }}</span></div>
            <div class="box scroll" style="flex:1;min-height:0">
              <div class="tbl-row th" style="grid-template-columns:96px minmax(0,1fr) minmax(0,1.1fr)"><span>{{ tr('Имя') }}</span><span>{{ tr('Описание') }}</span><span>{{ tr('Изменения') }}</span></div>
              <div v-for="r in impDiff.rows" :key="r.s.code + r.status" class="tbl-row" :class="{ dim: r.status === 'same' }" style="grid-template-columns:96px minmax(0,1fr) minmax(0,1.1fr)">
                <span class="mono" style="font-size:12px">{{ r.s.code }}</span>
                <span class="ell" :title="desc(r.s)">{{ desc(r.s) }}</span>
                <span class="st ell" :title="r.text"><span class="sd" :style="{ background: statusColor(r.status) }"></span><span class="ell">{{ r.text }}</span></span>
              </div>
            </div>
          </template>
          <div v-else class="drop-zone" style="flex:1;height:auto;border-style:solid;border-color:var(--line)"><span class="faint">{{ tr('Изменения появятся здесь') }}</span></div>
        </div>
      </div>
      <div class="dlg-f">
        <label class="chk"><input v-model="d.removeMissing" type="checkbox" class="cb">{{ tr('Удалить серверы, которых нет в файле') }}<span v-if="impDiff && impDiff.missing" class="faint">({{ impDiff.missing }})</span></label>
        <div class="grow"></div>
        <button class="btn ghost" @click="closeDlg(null)">{{ tr('Отмена') }}</button>
        <button class="btn pri" :disabled="!impDiff || !(impDiff.nNew + impDiff.nChanged + impDiff.nRemoved)" @click="applyImport">{{ tr('Импортировать') }} {{ impDiff ? impDiff.nNew + impDiff.nChanged + impDiff.nRemoved : '' }}</button>
      </div>
    </template>
    <template v-else>
      <div class="dlg-b" style="display:grid;grid-template-columns:260px minmax(0,1fr);gap:24px;height:470px">
        <div style="display:flex;flex-direction:column;gap:10px">
          <h3 class="h3" style="margin:0">{{ tr('Группы') }}</h3>
          <label v-for="g in sideGroups" :key="g.name" class="chk"><input v-model="d.exportGroups[g.name]" type="checkbox" class="cb"><span class="grow ell">{{ g.name }}</span><span class="faint">{{ g.total }}</span></label>
          <div class="sep" style="margin:6px 0"></div>
          <label class="chk"><input v-model="d.exportUsers" type="checkbox" class="cb">{{ tr('Пользователи и имена ключей') }}</label>
          <div class="faint" style="font-size:12px">{{ tr('Без паролей и ключей.') }}</div>
        </div>
        <pre class="code scroll" style="min-height:0">{{ exportJson }}</pre>
      </div>
      <div class="dlg-f">
        <span class="faint">{{ trn(exportCount, 'сервер|сервера|серверов') }}</span><div class="grow"></div>
        <button class="btn sec" @click="copyExport"><Icon n="copy" s="s" />{{ tr('Копировать') }}</button>
        <button class="btn pri" :disabled="!exportCount" @click="saveExport">{{ tr('Сохранить') }}</button>
      </div>
    </template>
  </div>
</template>
