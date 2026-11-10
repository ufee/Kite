<script setup lang="ts">
import { computed } from 'vue';
import { ICONS } from '../icons';

const props = defineProps<{ n: string; s?: 'xs' | 's' | 'l' }>();
// Bodies come from the static icon table only, never from data.
const body = computed(() => ICONS[props.n] ?? '');
</script>

<template>
  <svg :class="['ic', s]" viewBox="0 0 24 24" aria-hidden="true" v-html="body"></svg>
</template>
