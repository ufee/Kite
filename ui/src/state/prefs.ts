/* Interface preferences and settings. Kept in localStorage under the `kite.` prefix. */
import { computed, reactive, ref, watch } from 'vue';
import { lang, systemLang, type Lang } from '../i18n';

export const store = {
  get<T>(k: string, d: T): T {
    try { const v = localStorage.getItem('kite.' + k); return v == null ? d : (JSON.parse(v) as T); } catch { return d; }
  },
  set(k: string, v: unknown) {
    try { localStorage.setItem('kite.' + k, JSON.stringify(v)); } catch { /* storage may be blocked */ }
  },
};

export type View = 'files' | 'both' | 'term';
export type ColKey = 'size' | 'date' | 'perm' | 'owner' | 'group';

export const ui = reactive({
  theme: store.get<'dark' | 'light' | 'system'>('theme', 'dark'),
  view: store.get<View>('view', 'both'),
  sideW: store.get('sideW', 232),
  split: store.get('split', 0.5),
  collapsed: store.get('collapsed', false),
  termH: store.get('termH', 244),
  closed: store.get<Record<string, boolean>>('closedGroups', {}),
  sideSort: store.get<'name' | 'recent'>('sideSort', 'name'),
  search: '',
  cols: store.get<Record<ColKey, boolean>>('cols', { size: true, date: true, perm: true, owner: true, group: true }),
  hidden: store.get('hidden', true),
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
  autosave: boolean;
  editor: 'system' | 'npp' | 'code';
  lang: Lang;
}

export const settings = reactive<Settings>({
  parallel: 3, conflict: 'ask', keepTime: true, keyMode: 'auto', keyFile: '', agentFwd: false,
  strictHost: true, termFont: 13, shell: 'pwsh', copyOnSelect: true, autosave: true, editor: 'system',
  lang: systemLang(),
  ...store.get<Partial<Settings>>('settings', {}),
});

const sysDark = ref(matchMedia('(prefers-color-scheme: dark)').matches);
matchMedia('(prefers-color-scheme: dark)').addEventListener('change', e => { sysDark.value = e.matches; });
export const theme = computed(() => (ui.theme === 'system' ? (sysDark.value ? 'dark' : 'light') : ui.theme));
export function toggleTheme() { ui.theme = theme.value === 'dark' ? 'light' : 'dark'; }

watch(() => settings.lang, l => { lang.value = l; document.documentElement.lang = l; }, { immediate: true });
watch(theme, t => { document.documentElement.dataset.theme = t; }, { immediate: true });
watch(() => [ui.theme, ui.view, ui.sideW, ui.split, ui.collapsed, ui.termH, ui.sideSort, ui.hidden] as const, () => {
  store.set('theme', ui.theme); store.set('view', ui.view); store.set('sideW', ui.sideW); store.set('split', ui.split);
  store.set('collapsed', ui.collapsed); store.set('termH', ui.termH); store.set('sideSort', ui.sideSort); store.set('hidden', ui.hidden);
});
watch(() => ({ ...ui.cols }), v => store.set('cols', v));
watch(() => ({ ...ui.closed }), v => store.set('closedGroups', v));
watch(settings, v => store.set('settings', { ...v }), { deep: true });
