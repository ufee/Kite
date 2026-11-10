<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { backend } from '../../backend';
import { tr } from '../../i18n';
import { closeDlg } from '../../state/overlay';
import { sideGroups, srvBy } from '../../state/servers';
import { addUser, chooseKeyFile, deleteFromCard, passHint, removeUser, saveServer, testServer } from '../../state/serverCard';
import Icon from '../Icon.vue';

const props = defineProps<{ d: Record<string, any> }>();
const keys = ref<string[]>([]);
onMounted(async () => { keys.value = await backend.keysList(); });
const isDef = (name: string) => !!name && props.d.defUser === name;
function nameInput(k: number, name: string) {
  if (k === 0 && props.d.users.length === 1) props.d.defUser = name;
}
const testColor = () => ({ bad: 'var(--bad)', ok: 'var(--ok)' } as Record<string, string>)[props.d.test?.state] ?? 'var(--tx3)';
</script>

<template>
  <div class="dlg w720">
    <div class="dlg-h"><span class="dlg-t">{{ d.id ? tr('Сервер {code}', { code: srvBy(d.id)?.code ?? d.code }) : tr('Новый сервер') }}</span><div class="grow"></div><button class="ibtn" :title="tr('Закрыть') + ' · Esc'" @click="closeDlg(null)"><Icon n="x" /></button></div>
    <div class="dlg-b scroll">
      <div class="row2">
        <div><label class="lbl">{{ tr('Имя') }}</label><input v-model="d.code" class="fld mono" :class="{ err: d.errs.code }" placeholder="web1" spellcheck="false"><div v-if="d.errs.code" class="fld-err">{{ d.errs.code }}</div></div>
        <div><label class="lbl">{{ tr('Группа') }}</label><input v-model="d.group" class="fld" list="grp-list"><datalist id="grp-list"><option v-for="g in sideGroups" :key="g.name" :value="g.name"></option></datalist></div>
      </div>
      <div><label class="lbl">{{ tr('Описание') }}</label><input v-model="d.desc" class="fld" :placeholder="tr('Что на сервере')"></div>
      <div style="display:grid;grid-template-columns:minmax(0,1fr) 120px;gap:16px">
        <div><label class="lbl">{{ tr('Адрес') }}</label><input v-model="d.host" class="fld mono" :class="{ err: d.errs.host }" :placeholder="tr('IP или домен')" spellcheck="false"><div v-if="d.errs.host" class="fld-err">{{ d.errs.host }}</div></div>
        <div><label class="lbl">{{ tr('Порт') }}</label><input v-model.number="d.port" class="fld mono" :class="{ err: d.errs.port }" type="number" min="1" max="65535"></div>
      </div>
      <div>
        <h3 class="h3">{{ tr('Пользователи') }}</h3>
        <div class="ulist">
          <div v-for="(u, k) in d.users" :key="k" class="urow">
            <button class="ibtn ustar" :style="{ color: isDef(u.name) ? 'var(--ac)' : '' }" :title="tr(d.defUser === u.name ? 'По умолчанию' : 'Сделать по умолчанию')" @click="d.defUser = u.name"><Icon n="star" :style="{ fill: isDef(u.name) ? 'currentColor' : 'none' }" /></button>
            <input v-model="u.name" class="fld mono" :placeholder="tr('имя')" spellcheck="false" @input="nameInput(Number(k), u.name)">
            <select v-model="u.key" class="fld">
              <option value="auto">{{ tr('Ключ: авто') }}</option>
              <option v-for="key in keys" :key="key" :value="key">{{ key }}</option>
            </select>
            <button class="ibtn uremove" :title="tr('Убрать')" @click="removeUser(d, Number(k))"><Icon n="x" s="s" /></button>
          </div>
        </div>
        <div style="display:flex;align-items:center;gap:12px;margin-top:6px">
          <button class="textbtn" style="margin-left:-10px" @click="addUser(d)"><Icon n="plus" s="s" />{{ tr('Добавить пользователя') }}</button>
          <span v-if="d.errs.users" style="color:var(--bad);font-size:12px">{{ d.errs.users }}</span>
        </div>
      </div>
      <div>
        <h3 class="h3">{{ tr('Пароль') }}</h3>
        <div class="row2" style="align-items:center">
          <input v-model="d.pass" class="fld pass-in" :class="{ err: d.errs.pass }" type="password" :placeholder="d.hasPass ? tr('Пароль сохранён') : ''" autocomplete="new-password" spellcheck="false" @input="d.errs.pass = ''" @keydown.enter.prevent="saveServer(d)">
          <label class="chk" :class="{ off: (!d.pass && !d.hasPass) || d.noStore }"><input v-model="d.install" type="checkbox" class="cb" :disabled="(!d.pass && !d.hasPass) || d.noStore">{{ tr('Добавить мой SSH-ключ') }}</label>
        </div>
        <div v-if="d.errs.pass" style="color:var(--bad);font-size:12px;margin-top:6px">{{ d.errs.pass }}</div>
        <div v-else class="faint" style="font-size:12px;margin-top:6px">{{ passHint(d) }}</div>
      </div>
      <div>
        <h3 class="h3">{{ tr('Ключ по умолчанию') }}</h3>
        <div style="display:flex;flex-direction:column;gap:8px">
          <button class="opt" :class="{ on: d.keyMode === 'auto' }" @click="d.keyMode = 'auto'"><span class="radio"></span><span><span style="font-weight:500">{{ tr('Авто') }}</span><br><span class="muted" style="font-size:12px">{{ tr('Агент OpenSSH, затем id_ed25519') }}</span></span></button>
          <div class="opt" :class="{ on: d.keyMode === 'file' }" style="cursor:pointer" @click="d.keyMode = 'file'"><span class="radio"></span>
            <span class="grow"><span style="font-weight:500">{{ tr('Файл ключа') }}</span><br><span class="muted mono ell" style="font-size:12px;display:block">{{ d.keyFile || tr('не выбран') }}</span></span>
            <button class="btn sec" style="height:30px" @click.stop="chooseKeyFile(d)">{{ tr('Выбрать…') }}</button>
          </div>
        </div>
      </div>
      <div class="note"><Icon n="shield" :style="{ color: d.fp ? 'var(--ok)' : 'var(--tx3)' }" />
        <div v-if="d.fp"><div>{{ tr('Ключ хоста проверен') }}</div><div class="faint mono" style="font-size:12px">ED25519 · {{ d.fp }}</div></div>
        <div v-else class="muted">{{ tr('Ключ хоста ещё не проверен') }}</div>
      </div>
      <div>
        <button class="textbtn" style="margin-left:-10px" @click="d.adv = !d.adv"><Icon n="chev" s="xs" :style="{ transform: d.adv ? '' : 'rotate(-90deg)', transition: 'transform .2s' }" />{{ tr('Дополнительно') }}</button>
        <div v-if="d.adv" class="row2" style="margin-top:10px">
          <div><label class="lbl">{{ tr('Начальная папка') }}</label><input v-model="d.startDir" class="fld mono" :placeholder="tr('домашняя')" spellcheck="false"></div>
          <div><label class="lbl">{{ tr('Keepalive, с') }}</label><input v-model.number="d.keepalive" class="fld mono" type="number"></div>
          <div><label class="lbl">{{ tr('Через jump-хост') }}</label><select class="fld" disabled><option>{{ tr('Нет') }}</option></select></div>
        </div>
      </div>
    </div>
    <div class="dlg-f">
      <button v-if="d.id" class="btn dghost" @click="deleteFromCard(d)"><Icon n="trash" s="s" />{{ tr('Удалить') }}</button>
      <button class="btn sec" :disabled="d.test?.state === 'run'" :title="tr('Проверить подключение')" @click="testServer(d)">{{ tr('Проверить') }}</button>
      <span v-if="d.test" class="st test-st" :title="d.test.state === 'run' ? '' : d.test.text" :style="{ color: testColor() }">
        <span v-if="d.test.state === 'run'" class="spinner" style="width:14px;height:14px"></span><span class="ell">{{ d.test.state === 'run' ? d.test.text || tr('Проверка…') : d.test.text }}</span></span>
      <div class="grow"></div>
      <button class="btn ghost" @click="closeDlg(null)">{{ tr('Отмена') }}</button>
      <button class="btn pri" :disabled="d.busy" @click="saveServer(d)">{{ tr('Сохранить') }}</button>
    </div>
  </div>
</template>
