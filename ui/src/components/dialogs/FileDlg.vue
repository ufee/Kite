<script setup lang="ts">
import { tr } from '../../i18n';
import { closeDlg } from '../../state/overlay';
import { ui } from '../../state/prefs';
import { fileEdit } from '../../state/panes';
import Icon from '../Icon.vue';

// Rights are checked by the server on upload, so "Edit" is always offered.
defineProps<{ d: Record<string, any> }>();
</script>

<template>
  <div class="dlg w880 file">
    <div class="dlg-h">
      <span class="dlg-t ell" style="flex:none;max-width:45%">{{ d.name }}</span>
      <span class="file-where ell" :title="d.where">{{ d.where }}</span>
      <label class="chk file-wrap" :title="tr('Переносить длинные строки') + ' · Alt Z'"><input v-model="ui.wrap" type="checkbox" class="cb">{{ tr('Перенос строк') }}</label>
      <button class="btn sec" :title="tr('Открыть в редакторе') + ' · F4'" @click="fileEdit(d)">{{ tr('Редактировать') }}</button>
      <button class="ibtn" :title="tr('Закрыть') + ' · Esc'" @click="closeDlg(null)"><Icon n="x" /></button>
    </div>
    <textarea class="file-text" :class="{ wrap: ui.wrap }" :value="d.text" readonly :wrap="ui.wrap ? 'soft' : 'off'" spellcheck="false"></textarea>
  </div>
</template>
