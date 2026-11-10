<script setup lang="ts">
import { tr } from '../../i18n';
import { closeDlg } from '../../state/overlay';
import { HOTKEYS } from '../../state/keys';
import Icon from '../Icon.vue';

defineProps<{ d: Record<string, any> }>();
</script>

<template>
  <div class="dlg w560">
    <div class="dlg-h"><span class="dlg-t">{{ tr('Горячие клавиши') }}</span><div class="grow"></div><button class="ibtn" :title="tr('Закрыть')" @click="closeDlg(null)"><Icon n="x" /></button></div>
    <div class="dlg-b scroll" style="max-height:560px;display:grid;grid-template-columns:minmax(0,1fr) auto;gap:8px 24px;align-content:start">
      <template v-for="(k, t) in HOTKEYS" :key="t"><span class="muted">{{ tr(t) }}</span><span class="kbd" style="justify-self:end">{{ tr(k) }}</span></template>
    </div>
  </div>
</template>
