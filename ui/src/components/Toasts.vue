<script setup lang="ts">
import { tr } from '../i18n';
import { dropToast, toasts } from '../state/overlay';
import Icon from './Icon.vue';
</script>

<template>
  <transition-group name="toast" tag="div" class="toasts">
    <div v-for="t in toasts" :key="t.id" class="toast">
      <Icon :n="t.kind === 'bad' ? 'warn' : t.kind === 'info' ? 'info' : 'check'" :class="t.kind" />
      <span class="grow">{{ t.text }}</span>
      <button v-if="t.action" class="textbtn on" @click="t.action.run(); dropToast(t.id)">{{ t.action.label }}</button>
      <button class="ibtn" :title="tr('Скрыть')" @click="dropToast(t.id)"><Icon n="x" s="s" /></button>
    </div>
  </transition-group>
</template>
