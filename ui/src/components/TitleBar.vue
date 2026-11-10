<script setup lang="ts">
import { tr } from '../i18n';
import { theme, toggleTheme, ui } from '../state/prefs';
import { setView, VIEWS } from '../state/layout';
import { openPalette, openSettings } from '../state/palette';
import { windowBtn } from '../platform/window';
import Icon from './Icon.vue';

const VIEW_ICON = { files: 'vfiles', both: 'vboth', term: 'term' } as const;
</script>

<template>
  <!-- The empty parts of the bar move the window; Tauri also maximizes on double click there. -->
  <header class="titlebar" data-tauri-drag-region>
    <div class="tb-left" data-tauri-drag-region>
      <svg class="logo" viewBox="0 0 64 64" aria-hidden="true"><path fill-rule="evenodd" d="M32 3A29 29 0 1 1 32 61A29 29 0 1 1 32 3ZM48.68 31.96C48.68 33.91 46.34 35.08 43.22 35.47L39.32 36.25C41.66 42.1 41.66 49.12 37.76 55.36C34.64 50.68 30.74 46 28.79 36.64L25.28 36.25L11.24 39.76L17.48 31.96L11.24 24.16L25.28 27.67L28.79 27.28C30.74 17.92 34.64 13.24 37.76 8.56C41.66 14.8 41.66 21.82 39.32 27.67L43.22 28.45C46.34 28.84 48.68 30.01 48.68 31.96Z" /></svg>
      <span class="brand" data-tauri-drag-region>Kite</span>
    </div>
    <div class="tb-mid" data-tauri-drag-region>
      <button class="cmd" @click="openPalette()">
        <Icon n="search" s="s" /><span class="grow" style="text-align:left">{{ tr('Серверы, папки, команды') }}</span><span class="kbd">Ctrl K</span>
      </button>
      <button class="ibtn" :title="tr('Настройки') + ' · Ctrl ,'" @click="openSettings"><Icon n="gear" /></button>
    </div>
    <div class="tb-right" data-tauri-drag-region>
      <div class="vseg" role="group" :aria-label="tr('Вид')">
        <button v-for="(t, v, k) in VIEWS" :key="v" :class="{ on: ui.view === v }" :title="tr(t) + ' · Ctrl ' + (k + 1)" @click="setView(v)">
          <Icon :n="VIEW_ICON[v]" s="s" />
        </button>
      </div>
      <button class="ibtn" :title="tr(theme === 'dark' ? 'Светлая тема' : 'Тёмная тема')" @click="toggleTheme"><Icon :n="theme === 'dark' ? 'sun' : 'moon'" /></button>
      <button class="cap" style="margin-left:6px" :title="tr('Свернуть')" @click="windowBtn('min')"><Icon n="wmin" s="s" /></button>
      <button class="cap" :title="tr('Развернуть')" @click="windowBtn('max')"><Icon n="wmax" s="s" /></button>
      <button class="cap close" :title="tr('Закрыть')" @click="windowBtn('close')"><Icon n="x" s="s" /></button>
    </div>
  </header>
</template>
