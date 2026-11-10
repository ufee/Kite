<script setup lang="ts">
import { closeDlg, dlg } from '../state/overlay';
import ConfirmDlg from './dialogs/ConfirmDlg.vue';
import ConflictDlg from './dialogs/ConflictDlg.vue';
import HostKeyDlg from './dialogs/HostKeyDlg.vue';
import ServerDlg from './dialogs/ServerDlg.vue';
import ImportDlg from './dialogs/ImportDlg.vue';
import ChmodDlg from './dialogs/ChmodDlg.vue';
import SettingsDlg from './dialogs/SettingsDlg.vue';
import DestDlg from './dialogs/DestDlg.vue';
import FileDlg from './dialogs/FileDlg.vue';
import KeysDlg from './dialogs/KeysDlg.vue';

// A click on the backdrop closes only the dialogs that hold nothing to lose.
const LIGHT = ['confirm', 'keys', 'chmod', 'settings', 'dest', 'file'];
const VIEWS = { confirm: ConfirmDlg, conflict: ConflictDlg, hostkey: HostKeyDlg, server: ServerDlg, import: ImportDlg, chmod: ChmodDlg, settings: SettingsDlg, dest: DestDlg, file: FileDlg, keys: KeysDlg };
</script>

<template>
  <div v-if="dlg" class="scrim" @mousedown.self="LIGHT.includes(dlg.kind) && closeDlg(null)">
    <component :is="VIEWS[dlg.kind]" :key="dlg.id" :d="dlg.data" />
  </div>
</template>
