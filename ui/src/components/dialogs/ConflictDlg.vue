<script setup lang="ts">
import { tr } from '../../i18n';
import { closeDlg } from '../../state/overlay';
import { resolveConflict } from '../../state/queue';

defineProps<{ d: Record<string, any> }>();
</script>

<template>
  <div class="dlg w440">
    <div class="dlg-b" style="padding-top:24px;gap:10px">
      <div class="dlg-t">{{ d.names.length === 1 ? tr('Такой файл уже есть') : tr('Совпадают имена: {n}', { n: d.names.length }) }}</div>
      <div class="muted"><span class="mono" style="font-size:12px">{{ d.names.slice(0, 3).join(', ') }}</span>{{ d.names.length > 3 ? tr(' и ещё {n}', { n: d.names.length - 3 }) : '' }}{{ tr(' в {where}', { where: d.where }) }}</div>
      <label class="chk"><input v-model="d.remember" type="checkbox" class="cb">{{ tr('Запомнить выбор') }}</label>
    </div>
    <div class="dlg-f" style="border-top:0">
      <button class="btn ghost" @click="resolveConflict(d, 'skip', closeDlg)">{{ tr('Пропустить') }}</button><div class="grow"></div>
      <button class="btn sec" @click="resolveConflict(d, 'both', closeDlg)">{{ tr('Оставить оба') }}</button>
      <button class="btn pri" @click="resolveConflict(d, 'replace', closeDlg)">{{ tr('Заменить') }}</button>
    </div>
  </div>
</template>
