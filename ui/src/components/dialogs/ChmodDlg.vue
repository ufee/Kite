<script setup lang="ts">
import { tr } from '../../i18n';
import { objs } from '../../util/format';
import { closeDlg } from '../../state/overlay';
import { applyChmod, octal, setOctal } from '../../state/panes';
import Icon from '../Icon.vue';

defineProps<{ d: Record<string, any> }>();
const ROWS = ['Владелец', 'Группа', 'Остальные'];
</script>

<template>
  <div class="dlg w440">
    <div class="dlg-h"><span class="dlg-t ell">{{ tr('Права') }} · {{ d.names.length === 1 ? d.names[0] : objs(d.names.length) }}</span><div class="grow"></div><button class="ibtn" :title="tr('Закрыть')" @click="closeDlg(null)"><Icon n="x" /></button></div>
    <div class="dlg-b">
      <div class="perm-grid">
        <span></span><span class="faint" style="font-size:12px">{{ tr('Чтение') }}</span><span class="faint" style="font-size:12px">{{ tr('Запись') }}</span><span class="faint" style="font-size:12px">{{ tr('Запуск') }}</span>
        <template v-for="(g, r) in (d.bits as boolean[][])" :key="r">
          <span class="muted">{{ tr(ROWS[r]!) }}</span>
          <label v-for="(_, c) in g" :key="c"><input v-model="d.bits[r][c]" type="checkbox" class="cb"></label>
        </template>
      </div>
      <div style="display:grid;grid-template-columns:100px minmax(0,1fr) minmax(0,1fr);gap:12px">
        <div><label class="lbl">{{ tr('Код') }}</label><input class="fld mono" :value="octal(d.bits)" maxlength="3" @input="setOctal(d, ($event.target as HTMLInputElement).value)"></div>
        <div><label class="lbl">{{ tr('Владелец') }}</label><input v-model="d.owner" class="fld mono" spellcheck="false"></div>
        <div><label class="lbl">{{ tr('Группа') }}</label><input v-model="d.group" class="fld mono" spellcheck="false"></div>
      </div>
      <label v-if="d.hasDir" class="chk"><input v-model="d.recursive" type="checkbox" class="cb">{{ tr('Применить к вложенным') }}</label>
    </div>
    <div class="dlg-f"><div class="grow"></div><button class="btn ghost" @click="closeDlg(null)">{{ tr('Отмена') }}</button><button class="btn pri" :disabled="d.busy" @click="applyChmod(d)">{{ tr('Применить') }}</button></div>
  </div>
</template>
