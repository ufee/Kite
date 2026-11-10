/* Interface preferences and settings. They are read from settings.json before the first paint (see persist.ts). */
import { computed, reactive, ref, watch } from 'vue';
import { lang, systemLang, type Lang } from '../i18n';

export type View = 'files' | 'both' | 'term';
export type ColKey = 'size' | 'date' | 'perm' | 'owner' | 'group';

export const ui = reactive({
  theme: 'dark' as 'dark' | 'light' | 'system',
  view: 'both' as View,
  sideW: 232,
  split: 0.5,
  collapsed: false,
  termH: 244,
  closed: {} as Record<string, boolean>,
  sideSort: 'name' as 'name' | 'recent',
  search: '',
  cols: { size: true, date: true, perm: true, owner: true, group: true } as Record<ColKey, boolean>,
  hidden: true,
  focus: 0,
  drag: null as null | 'side' | 'split' | 'term',
  dnd: null as null | { from: number; names: string[] },
  stackH: 600,
  noanim: false,
  queueOpen: false,
  logOpen: false,
  logErr: false,
  logOnlyErr: false,
  vw: innerWidth,
  vh: innerHeight,
  copied: null as null | number,
  term: null as null | number,
  sideDocked: false,
});

/** The part of `ui` that settings.json keeps: look and layout, not what is open right now. */
export const UI_KEPT = ['theme', 'view', 'sideW', 'split', 'collapsed', 'termH', 'closed', 'sideSort', 'cols', 'hidden'] as const;

export interface Settings {
  parallel: number;
  conflict: 'ask' | 'replace' | 'both' | 'skip';
  keepTime: boolean;
  keyMode: 'auto' | 'file';
  keyFile: string;
  agentFwd: boolean;
  strictHost: boolean;
  termFont: number;
  shell: 'pwsh' | 'powershell' | 'cmd';
  copyOnSelect: boolean;
  saveTermHistory: boolean;
  autosave: boolean;
  editor: 'system' | 'npp' | 'code';
  lang: Lang;
}

export const settings = reactive<Settings>({
  parallel: 3, conflict: 'ask', keepTime: true, keyMode: 'auto', keyFile: '', agentFwd: false,
  strictHost: true, termFont: 13, shell: 'pwsh', copyOnSelect: true, saveTermHistory: false, autosave: true, editor: 'system',
  lang: systemLang(),
});

const sysDark = ref(matchMedia('(prefers-color-scheme: dark)').matches);
matchMedia('(prefers-color-scheme: dark)').addEventListener('change', e => { sysDark.value = e.matches; });
export const theme = computed(() => (ui.theme === 'system' ? (sysDark.value ? 'dark' : 'light') : ui.theme));
export function toggleTheme() { ui.theme = theme.value === 'dark' ? 'light' : 'dark'; }

watch(() => settings.lang, l => { lang.value = l; document.documentElement.lang = l; }, { immediate: true });
watch(theme, t => { document.documentElement.dataset.theme = t; }, { immediate: true });
