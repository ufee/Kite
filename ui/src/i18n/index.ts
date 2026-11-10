import { ref } from 'vue';
import { EN } from './en';

export type Lang = 'ru' | 'en';

/** UI language. Russian strings are the message ids, EN maps them to English. */
export const lang = ref<Lang>('ru');

/** Cyrillic strings without a translation, for checking coverage in dev. */
export const trMissing = new Set<string>();

export const systemLang = (): Lang => (navigator.language.toLowerCase().startsWith('ru') ? 'ru' : 'en');

export function tr(s: string, vars?: Record<string, string | number>): string {
  let out = s;
  if (lang.value === 'en') {
    out = EN[s] ?? s;
    if (out === s && /[а-яё]/i.test(s)) trMissing.add(s);
  }
  return vars ? out.replace(/\{(\w+)\}/g, (_, k: string) => String(vars[k] ?? '')) : out;
}

export function plural(n: number, one: string, few: string, many: string): string {
  const a = n % 10, b = n % 100;
  if (a === 1 && b !== 11) return one;
  if (a >= 2 && a <= 4 && (b < 12 || b > 14)) return few;
  return many;
}

/** forms: 'сервер|сервера|серверов'; English has 'server|servers'. */
export function trn(n: number, forms: string): string {
  const f = tr(forms).split('|');
  return `${n} ${lang.value === 'en' ? f[n === 1 ? 0 : 1] : plural(n, f[0], f[1], f[2])}`;
}

if (import.meta.env.DEV) (window as unknown as { trMissing: Set<string> }).trMissing = trMissing;
