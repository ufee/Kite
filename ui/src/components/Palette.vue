<script setup lang="ts">
import { tr } from '../i18n';
import { closePalette, pal, palItems, palKey, runPal } from '../state/palette';
import Icon from './Icon.vue';
</script>

<template>
  <div v-if="pal.open" class="scrim top" @mousedown.self="closePalette">
    <div class="pal">
      <div class="pal-in"><Icon n="search" /><input v-model="pal.q" :placeholder="tr('Сервер, папка или команда')" spellcheck="false" @keydown="palKey"><span class="faint" style="font-size:12px;white-space:nowrap">{{ tr('@ пользователь · > команды') }}</span></div>
      <div class="pal-list scroll">
        <template v-for="(it, k) in palItems" :key="k">
          <div v-if="k === 0 || palItems[k - 1].sec !== it.sec" class="pal-h">{{ it.sec }}</div>
          <button class="pal-it" :class="{ on: k === pal.idx }" @mousemove="pal.idx = k" @click="runPal(k, $event.altKey)">
            <Icon :n="it.icon" class="ico" />
            <span :class="it.kind === 'srv' ? 'mono' : ''" style="white-space:nowrap">{{ it.title }}</span>
            <span class="faint ell grow">{{ it.sub }}</span>
            <span v-if="it.live" class="live" :title="tr('Подключён')"></span>
            <span v-if="it.hint" class="kbd">{{ it.hint }}</span>
          </button>
        </template>
        <div v-if="!palItems.length" class="empty-note">{{ tr('Ничего не найдено') }}</div>
      </div>
      <div class="pal-f"><span><span class="kbd">↑↓</span>{{ tr('выбрать') }}</span><span><span class="kbd">Enter</span>{{ tr('открыть') }}</span><span><span class="kbd">Alt Enter</span>{{ tr('в другой панели') }}</span><span><span class="kbd">Esc</span>{{ tr('закрыть') }}</span></div>
    </div>
  </div>
</template>
