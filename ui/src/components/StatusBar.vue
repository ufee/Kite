<script setup lang="ts">
import { tr } from '../i18n';
import { copyText, fmtSpeed } from '../util/format';
import { ui } from '../state/prefs';
import { toast } from '../state/overlay';
import { toggleLog } from '../state/log';
import { idleInfo } from '../state/panes';
import { active, pauseAll, qErr, qStat, queue, routeLabel } from '../state/queue';
import Icon from './Icon.vue';
import Wave from './Wave.vue';
import LogWin from './LogWin.vue';
import QueueWin from './QueueWin.vue';

// v26.11.0 shows as v26.11; a build not from the tag adds its commit: v26.11 · a1b2c3d.
const VERSION = __APP_VERSION__, SHORT = 'v' + VERSION.replace(/\.0$/, '') + (__APP_COMMIT__ ? ' · ' + __APP_COMMIT__ : '');
async function copyVersion() {
  await copyText(`Kite v${VERSION}${__APP_COMMIT__ ? ' (' + __APP_COMMIT__ + ')' : ''} · Windows x64`);
  toast('Версия скопирована', 'ok', null, 1800);
}
</script>

<template>
  <footer class="status">
    <template v-if="qStat">
      <Icon n="arrows" s="s" class="faint" />
      <span class="ell" style="max-width:240px">{{ qStat.cur.tname }}</span>
      <span class="faint ell" style="max-width:240px">{{ routeLabel(qStat.cur) }}</span>
      <div class="bar" :class="{ paused: qStat.paused }"><i :style="{ width: qStat.pct + '%' }"></i></div>
      <span class="muted" style="font-variant-numeric:tabular-nums">{{ Math.floor(qStat.pct) }}%</span>
      <span class="faint" style="font-variant-numeric:tabular-nums;white-space:nowrap">{{ qStat.paused ? tr('пауза') : fmtSpeed(qStat.speed) }}</span>
      <button class="ibtn" :title="tr(qStat.paused ? 'Продолжить все' : 'Приостановить все')" @click="pauseAll"><Icon :n="qStat.paused ? 'play' : 'pause'" s="s" /></button>
    </template>
    <span v-else class="faint ell">{{ idleInfo() }}</span>
    <div class="grow"></div>
    <button v-if="qErr.length" class="textbtn" style="color:var(--bad)" @click="ui.queueOpen = !ui.queueOpen"><Icon n="warn" s="s" />{{ tr('Ошибок: {n}', { n: qErr.length }) }}</button>
    <button v-if="queue.length" class="textbtn" :class="{ on: ui.queueOpen }" :title="tr('Очередь передач')" @click="ui.queueOpen = !ui.queueOpen">{{ tr('Очередь') }}<span class="faint">{{ active.length }}</span></button>
    <button class="textbtn dbg-btn" :class="{ on: ui.logOpen, alert: ui.logErr }" :title="tr(ui.logErr ? 'Есть новые ошибки' : 'Журнал операций')" @click="toggleLog"><Wave />{{ tr('Отладка') }}</button>
    <button class="textbtn ver-btn" :title="'Kite v' + VERSION + ' · ' + tr('скопировать')" @click="copyVersion">{{ SHORT }}</button>

    <LogWin v-if="ui.logOpen" />
    <QueueWin v-if="ui.queueOpen" />
  </footer>
</template>
