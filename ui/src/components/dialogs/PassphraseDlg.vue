<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { backend, isBackendError } from '../../backend';
import { tr } from '../../i18n';
import { closeDlg } from '../../state/overlay';
import Icon from '../Icon.vue';

/*
 * The core waits for the phrase of a key that is not in the agent. A wrong phrase keeps the dialog
 * with the error under the field; any other close (Esc, Cancel) is answered as a cancel by app.ts.
 */
const props = defineProps<{ d: Record<string, any> }>();
const input = ref<HTMLInputElement | null>(null);
onMounted(() => input.value?.focus());

async function open() {
  const d = props.d;
  if (d.busy || !d.phrase) return;
  d.busy = true; d.err = '';
  try {
    await backend.passphraseRespond(d.reqId, d.phrase, d.agent && d.add);
    d.phrase = '';
    closeDlg('ok');
  } catch (e) {
    d.phrase = '';
    if (isBackendError(e) && e.code === 'PASSPHRASE') {
      d.err = e.title;
      input.value?.focus();
    } else closeDlg(null); // the request is gone: the connection already ended
  } finally {
    d.busy = false;
  }
}
</script>

<template>
  <div class="dlg w440">
    <div class="dlg-h"><Icon n="key" style="color:var(--ac)" />
      <span class="dlg-t ell">{{ tr('Ключ {k} защищён паролем', { k: d.name }) }}</span></div>
    <div class="dlg-b">
      <div class="muted">{{ tr('Фраза нужна для входа {u}@{c}. Kite её не хранит.', { u: d.user, c: d.code }) }}</div>
      <div>
        <input ref="input" v-model="d.phrase" class="fld" :class="{ err: d.err }" type="password" :placeholder="tr('Парольная фраза')"
          autocomplete="off" spellcheck="false" :disabled="d.busy" @input="d.err = ''" @keydown.enter.prevent="open">
        <div v-if="d.err" style="color:var(--bad);font-size:12px;margin-top:6px">{{ d.err }}</div>
        <div v-else class="faint mono ell" style="font-size:12px;margin-top:6px">{{ d.file }}</div>
      </div>
      <div>
        <label class="chk" :class="{ off: !d.agent }"><input v-model="d.add" type="checkbox" class="cb" :disabled="!d.agent">{{ tr('Добавить в агент OpenSSH') }}</label>
        <div class="faint" style="font-size:12px;margin-top:6px">
          {{ !d.agent ? tr('Служба ssh-agent выключена. Ключ останется в памяти Kite до закрытия') : d.add ? tr('Фраза больше не понадобится: ключ возьмут из агента и Kite, и ssh, и git') : tr('Ключ останется в памяти Kite до закрытия') }}
        </div>
      </div>
    </div>
    <div class="dlg-f">
      <div class="grow"></div>
      <button class="btn ghost" @click="closeDlg(null)">{{ tr('Отмена') }}</button>
      <button class="btn pri" :disabled="!d.phrase || d.busy" @click="open">{{ tr('Открыть ключ') }}</button>
    </div>
  </div>
</template>
