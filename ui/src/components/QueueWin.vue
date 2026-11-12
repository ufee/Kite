<script setup lang="ts">
import type { TransferItem } from '../backend';
import { tr } from '../i18n';
import { fmtSize, fmtSpeed } from '../util/format';
import { ui } from '../state/prefs';
import { active, cancelQ, clearDone, qPct, queue, retryQ, routeLabel, togglePause } from '../state/queue';
import { WIN_EDGES, winDrag, winRaise, winResize, winStyle } from '../state/layout';
import Icon from './Icon.vue';

const live = (q: TransferItem) => ['run', 'count', 'wait', 'pause'].includes(q.state);
function progress(q: TransferItem) {
  switch (q.state) {
    case 'done': return tr('готово') + ' · ' + fmtSize(q.size);
    case 'pause': return tr('пауза') + ' · ' + qPct(q) + '%';
    case 'wait': return tr('в очереди');
    case 'count': return tr('подсчёт…');
    default: return qPct(q) + '% · ' + fmtSpeed(q.speed);
  }
}
</script>

<template>
  <div class="pop" :style="winStyle('queue')" @pointerdown.capture="winRaise('queue')">
    <div class="pop-h" @pointerdown="winDrag('queue', $event)">
      <span style="font-weight:600">{{ tr('Передачи') }}</span><span class="faint">{{ tr('В работе: {n}', { n: active.length }) }}</span>
      <div class="grow"></div>
      <button class="textbtn" @click="clearDone">{{ tr('Убрать готовые') }}</button>
      <button class="ibtn" :title="tr('Закрыть')" @click="ui.queueOpen = false"><Icon n="x" s="s" /></button>
    </div>
    <div class="scroll" style="flex:1;min-height:0">
      <div v-for="q in queue" :key="q.id" class="q-item">
        <div class="ell"><span :style="{ color: q.state === 'done' ? 'var(--tx2)' : '' }">{{ q.tname }}</span>&ensp;<span class="faint" style="font-size:12px">{{ routeLabel(q) }}</span></div>
        <div class="q-acts">
          <button v-if="live(q) && q.state !== 'count'" class="ibtn" :title="tr(q.state === 'pause' ? 'Продолжить' : 'Пауза')" @click="togglePause(q)"><Icon :n="q.state === 'pause' ? 'play' : 'pause'" s="s" /></button>
          <button v-if="q.state === 'err' && !q.twin" class="textbtn" @click="retryQ(q)">{{ tr('Повторить') }}</button>
          <button v-if="q.state === 'err' && q.denied" class="textbtn" @click="retryQ(q, true)">{{ tr('От root') }}</button>
          <button class="ibtn" :title="tr(q.state === 'done' ? 'Убрать из списка' : 'Отменить')" @click="cancelQ(q)"><Icon n="x" s="s" /></button>
        </div>
        <div v-if="q.state === 'err'" style="color:var(--bad);font-size:12px;line-height:16px">{{ q.err }}</div>
        <div v-else style="display:flex;align-items:center;gap:10px">
          <div class="bar" :class="{ paused: q.state === 'pause', count: q.state === 'count' }" style="flex:1"><i :style="{ width: qPct(q) + '%' }"></i></div>
          <span class="faint" style="font-size:12px;min-width:118px;text-align:right;font-variant-numeric:tabular-nums">{{ progress(q) }}</span>
        </div>
      </div>
      <div v-if="!queue.length" class="empty-note">{{ tr('Передач нет') }}</div>
    </div>
    <i v-for="d in WIN_EDGES" :key="d" class="rz" :class="d" @pointerdown="winResize('queue', d, $event)"></i>
  </div>
</template>
