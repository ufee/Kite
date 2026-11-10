<script setup lang="ts">
import { tr } from '../../i18n';
import { closeDlg } from '../../state/overlay';
import Icon from '../Icon.vue';

// The answer goes back to the core from the subscription that opened this dialog.
defineProps<{ d: Record<string, any> }>();
</script>

<template>
  <div class="dlg w560">
    <div class="dlg-h"><Icon :n="d.mode === 'changed' ? 'warn' : 'shield'" :style="{ color: d.mode === 'changed' ? 'var(--bad)' : 'var(--ac)' }" />
      <span class="dlg-t">{{ tr(d.mode === 'changed' ? 'Ключ хоста {code} изменился' : 'Первое подключение к {code}', { code: d.code }) }}</span></div>
    <div class="dlg-b">
      <template v-if="d.mode === 'changed'">
        <div class="note bad"><Icon n="warn" /><div>{{ tr('Сервер {host} прислал не тот ключ, что в known_hosts. Так бывает после переустановки или при перехвате соединения.', { host: d.host }) }}</div></div>
        <div><span class="lbl">{{ tr('Сохранённый') }}</span><div class="fp">{{ d.algo }} · {{ d.old }}</div></div>
        <div><span class="lbl">{{ tr('Новый') }}</span><div class="fp">{{ d.algo }} · {{ d.fp }}</div></div>
      </template>
      <template v-else>
        <div v-if="d.others" class="note bad"><Icon n="warn" /><div>{{ tr('Для {host} в known_hosts уже есть ключ другого типа. Сверь отпечаток, прежде чем доверять.', { host: d.host }) }}</div></div>
        <div class="muted">{{ tr(d.others ? 'Сверить отпечаток можно так:' : 'Новый сервер. Сверить отпечаток можно так:') }} <span class="mono" style="font-size:12px">ssh-keygen -lf /etc/ssh/ssh_host_{{ d.algo.toLowerCase() }}_key.pub</span></div>
        <div><span class="lbl">{{ d.host }}:{{ d.port }}</span><div class="fp">{{ d.algo }} · {{ d.fp }}</div></div>
      </template>
    </div>
    <div class="dlg-f">
      <template v-if="d.mode === 'changed'">
        <button class="btn dghost" @click="closeDlg('trust')">{{ tr('Доверять новому ключу') }}</button><div class="grow"></div>
        <button class="btn pri" @click="closeDlg(null)">{{ tr('Отключиться') }}</button>
      </template>
      <template v-else>
        <div class="grow"></div>
        <button class="btn ghost" @click="closeDlg(null)">{{ tr('Отмена') }}</button>
        <button class="btn pri" @click="closeDlg('trust')">{{ tr('Доверять и подключиться') }}</button>
      </template>
    </div>
  </div>
</template>
