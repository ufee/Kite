<script setup lang="ts">
import { tr } from '../../i18n';
import { closeDlg } from '../../state/overlay';

defineProps<{ d: Record<string, any> }>();
</script>

<template>
  <div class="dlg w440">
    <div class="dlg-b" style="padding-top:24px;gap:8px"><div class="dlg-t">{{ d.title }}</div><div class="muted">{{ d.text }}</div></div>
    <div class="dlg-f" style="border-top:0"><div class="grow"></div>
      <button class="btn ghost" @click="closeDlg(false)">{{ tr('Отмена') }}</button>
      <button v-if="d.alt" class="btn" @click="closeDlg('alt')">{{ d.alt }}</button>
      <button class="btn" :class="d.danger ? 'danger' : 'pri'" @click="closeDlg(d.alt ? 'ok' : true)">{{ d.ok }}</button>
    </div>
  </div>
</template>
