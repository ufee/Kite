<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { backend, type AgentStatus } from '../../backend';
import { tr } from '../../i18n';
import { settings, ui } from '../../state/prefs';
import { closeDlg } from '../../state/overlay';
import { VIEWS } from '../../state/layout';
import { openKeys } from '../../state/palette';
import { openImport } from '../../state/importExport';
import Icon from '../Icon.vue';

defineProps<{ d: Record<string, any> }>();
const agent = ref<AgentStatus | null>(null);
// Editors that are not installed stay in the list but cannot be picked.
const editors = ref<string[]>(['system', 'npp', 'code']);
onMounted(async () => {
  backend.editEditors().then(list => { editors.value = list; }).catch(() => undefined);
  agent.value = await backend.agentStatus();
});
const VIEW_SHORT = { files: 'Файлы', both: 'Оба', term: 'Терминал' } as const;
</script>

<template>
  <div class="dlg w560">
    <div class="dlg-h"><span class="dlg-t">{{ tr('Настройки') }}</span><div class="grow"></div><button class="ibtn" :title="tr('Закрыть') + ' · Esc'" @click="closeDlg(null)"><Icon n="x" /></button></div>
    <div class="dlg-b scroll" style="max-height:600px;gap:24px">
      <div>
        <h3 class="h3">{{ tr('Вид') }}</h3>
        <div class="set-grid">
          <span class="muted">{{ tr('Язык') }}</span>
          <div><div class="seg"><button :class="{ on: settings.lang === 'ru' }" @click="settings.lang = 'ru'">Русский</button><button :class="{ on: settings.lang === 'en' }" @click="settings.lang = 'en'">English</button></div></div>
          <span class="muted">{{ tr('Тема') }}</span>
          <div><div class="seg"><button :class="{ on: ui.theme === 'dark' }" @click="ui.theme = 'dark'">{{ tr('Тёмная') }}</button><button :class="{ on: ui.theme === 'light' }" @click="ui.theme = 'light'">{{ tr('Светлая') }}</button><button :class="{ on: ui.theme === 'system' }" @click="ui.theme = 'system'">{{ tr('Как в системе') }}</button></div></div>
          <span class="muted">{{ tr('Окно') }}</span>
          <div><div class="seg"><button v-for="(_, v) in VIEWS" :key="v" :class="{ on: ui.view === v }" @click="ui.view = v">{{ tr(VIEW_SHORT[v]) }}</button></div></div>
          <span class="muted">{{ tr('Шрифт терминала') }}</span>
          <div><select v-model.number="settings.termFont" class="fld" style="width:120px"><option v-for="n in [12, 13, 14, 15, 16]" :key="n" :value="n">{{ n }} px</option></select></div>
        </div>
      </div>
      <div>
        <h3 class="h3">{{ tr('Подключения') }}</h3>
        <div class="note" style="margin-bottom:12px">
          <Icon n="key" :style="{ color: agent?.running ? 'var(--ok)' : 'var(--tx3)' }" />
          <div v-if="!agent" class="muted">{{ tr('Проверяем агент OpenSSH…') }}</div>
          <div v-else-if="agent.running"><div>{{ tr('Агент OpenSSH запущен') }}</div><div class="faint" style="font-size:12px">{{ tr('Ключей: {n}', { n: agent.keys.length }) }}<template v-if="agent.keys.length"> · {{ agent.keys.join(', ') }}</template></div></div>
          <div v-else class="muted">{{ tr('Агент OpenSSH не запущен') }}</div>
        </div>
        <div style="display:flex;flex-direction:column;gap:10px">
          <label class="chk"><input v-model="settings.strictHost" type="checkbox" class="cb">{{ tr('Проверять ключи хостов') }}</label>
          <div v-if="!settings.strictHost" class="note bad" style="margin-left:24px"><Icon n="warn" /><div style="font-size:12px">{{ tr('Без проверки подмена сервера останется незамеченной.') }}</div></div>
          <label class="chk"><input v-model="settings.agentFwd" type="checkbox" class="cb">{{ tr('Копировать между серверами напрямую') }}</label>
          <div class="faint" style="font-size:12px;margin:-6px 0 0 24px">{{ tr('Файлы идут со сервера на сервер, минуя этот ПК') }}</div>
          <div v-if="settings.agentFwd" class="note bad" style="margin-left:24px"><Icon n="warn" /><div style="font-size:12px">{{ tr('Пока открыта сессия, root на сервере сможет подключаться с этим ключом.') }}</div></div>
        </div>
      </div>
      <div>
        <h3 class="h3">{{ tr('Серверы') }}</h3>
        <button class="btn sec" @click="openImport()"><Icon n="swap" s="s" />{{ tr('Импорт и экспорт…') }}</button>
      </div>
      <div>
        <h3 class="h3">{{ tr('Передачи') }}</h3>
        <div class="set-grid">
          <span class="muted">{{ tr('Одновременно') }}</span>
          <div><select v-model.number="settings.parallel" class="fld" style="width:120px"><option v-for="n in [1, 2, 3, 4, 5]" :key="n" :value="n">{{ n }}</option></select></div>
          <span class="muted">{{ tr('Если имя занято') }}</span>
          <div><select v-model="settings.conflict" class="fld" style="width:220px"><option value="ask">{{ tr('Спрашивать') }}</option><option value="replace">{{ tr('Заменять') }}</option><option value="both">{{ tr('Оставлять оба') }}</option><option value="skip">{{ tr('Пропускать') }}</option></select></div>
        </div>
        <label class="chk" style="margin-top:12px"><input v-model="settings.keepTime" type="checkbox" class="cb">{{ tr('Не менять дату файлов при копировании') }}</label>
      </div>
      <div>
        <h3 class="h3">{{ tr('Редактор') }}</h3>
        <div class="set-grid">
          <span class="muted">{{ tr('Программа') }}</span>
          <div><select v-model="settings.editor" class="fld" style="width:220px"><option value="system">{{ tr('Системный') }}</option><option value="npp" :disabled="!editors.includes('npp')">Notepad++</option><option value="code" :disabled="!editors.includes('code')">VS Code</option></select></div>
        </div>
        <label class="chk" style="margin-top:12px"><input v-model="settings.autosave" type="checkbox" class="cb">{{ tr('Сразу загружать правки на сервер') }}</label>
      </div>
      <div>
        <h3 class="h3">{{ tr('Терминал') }}</h3>
        <div class="set-grid">
          <span class="muted">{{ tr('Локальная оболочка') }}</span>
          <div><select v-model="settings.shell" class="fld" style="width:220px"><option value="pwsh">PowerShell 7</option><option value="powershell">Windows PowerShell</option><option value="cmd">{{ tr('Командная строка') }}</option></select></div>
        </div>
        <div style="display:flex;flex-direction:column;gap:10px;margin-top:12px">
          <label class="chk"><input v-model="settings.copyOnSelect" type="checkbox" class="cb">{{ tr('Копировать выделенное') }}</label>
          <label class="chk"><input v-model="settings.saveTermHistory" type="checkbox" class="cb">{{ tr('Сохранять историю терминала') }}</label>
          <div class="faint" style="font-size:12px;margin:-6px 0 0 24px">{{ tr('Последние 1000 строк каждой вкладки, только на этом ПК') }}</div>
        </div>
      </div>
    </div>
    <div class="dlg-f"><button class="textbtn" style="margin-left:-10px" @click="openKeys"><Icon n="keyboard" s="s" />{{ tr('Горячие клавиши') }}</button><div class="grow"></div><button class="btn pri" @click="closeDlg(null)">{{ tr('Готово') }}</button></div>
  </div>
</template>
