import { lang, tr, trn } from '../i18n';

export const KB = 1024, MB = KB * 1024, GB = MB * 1024;
export const MIN = 6e4, HOUR = 36e5, DAY = 864e5;

const NF1 = {
  ru: new Intl.NumberFormat('ru-RU', { maximumFractionDigits: 1 }),
  en: new Intl.NumberFormat('en-US', { maximumFractionDigits: 1 }),
};
export const collator = new Intl.Collator('ru', { numeric: true, sensitivity: 'base' });

export function fmtSize(b: number): string {
  if (b < KB) return Math.round(b) + ' ' + tr('Б');
  if (b < MB) return Math.round(b / KB) + ' ' + tr('КБ');
  const nf = NF1[lang.value];
  if (b < GB) return nf.format(b / MB) + ' ' + tr('МБ');
  return nf.format(b / GB) + ' ' + tr('ГБ');
}
export const fmtSpeed = (b: number) => fmtSize(b) + tr('/с');

export const pad = (x: number) => String(x).padStart(2, '0');
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

export function fmtDate(t: number): string {
  const d = new Date(t), n = new Date(), en = lang.value === 'en';
  const dm = en ? `${MONTHS[d.getMonth()]} ${pad(d.getDate())}` : pad(d.getDate()) + '.' + pad(d.getMonth() + 1);
  if (d.getFullYear() === n.getFullYear()) return `${dm} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  return `${dm}${en ? ' ' : '.'}${d.getFullYear()}`;
}

export const objs = (n: number) => trn(n, 'объект|объекта|объектов');

export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const ta = document.createElement('textarea');
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    try { document.execCommand('copy'); } catch { /* ignore */ }
    ta.remove();
    return true;
  }
}

export const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(v, hi));
export const sleep = (ms: number) => new Promise<void>(r => setTimeout(r, ms));
