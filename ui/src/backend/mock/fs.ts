/* In-memory file systems for the mock. The server side checks permissions, like a real sshd would. */
import type { BackendError, Entry, ErrCode, Src } from '../types';
import { DAY, KB } from '../../util/format';
import { baseName, childOf, homeOf, isLocal, LOCAL_WIN, parentOf } from '../../util/paths';
import { bigDir, ld, lf, LOCAL_SEED, POOL, rd, REMOTE_SEED, rf } from './data';
import { tr } from '../../i18n';

export function fail(code: ErrCode, title: string, text = ''): BackendError {
  return { code, title, text };
}

function hash(s: string): number {
  let h = 2166136261;
  for (const c of s) { h ^= c.codePointAt(0)!; h = Math.imul(h, 16777619); }
  return h >>> 0;
}
export function rng(seed: number) {
  let a = seed;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
export function fingerprint(seed: string): string {
  const r = rng(hash(seed)), abc = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  let s = 'SHA256:';
  for (let i = 0; i < 43; i++) s += abc[Math.floor(r() * 64)];
  return s;
}
export { hash };

/** Linux "This PC" has its own small tree under the home folder. */
const LINUX_LOCAL: Record<string, Entry[]> = {
  '/': [ld('home', 30 * DAY), ld('tmp', DAY)],
  '/home': [ld('user', DAY)],
  '/home/user': [ld('Downloads', DAY), ld('Projects', DAY), lf('.bashrc', 3.7 * KB, 90 * DAY)],
};

/** Server trees: one per server id, all grown from the same seed. */
export class MockFs {
  private dirs = new Map<string, Entry[]>();

  private key(src: Src, path: string) { return src + '|' + path; }

  private gen(src: Src, path: string): Entry[] {
    const r = rng(hash(src + '|' + path)), local = isLocal(src);
    const pick = (arr: string[], n: number) => {
      const a = [...arr], out: string[] = [];
      while (out.length < n && a.length) out.push(a.splice(Math.floor(r() * a.length), 1)[0]);
      return out;
    };
    const owner = local ? '' : (path.match(/^\/home\/([^/]+)/)?.[1] || 'root');
    const dirs = pick(local ? POOL.ldir : POOL.rdir, 1 + Math.floor(r() * 4)).map(n => (local ? ld(n, r() * 20 * DAY) : rd(n, r() * 20 * DAY, owner)));
    const files = pick(local ? POOL.lfile : POOL.rfile, 2 + Math.floor(r() * 6)).map(n => {
      const size = 200 + r() * r() * 900 * KB, ago = r() * 30 * DAY;
      return local ? lf(n, size, ago) : rf(n, size, ago, owner);
    });
    return [...dirs, ...files];
  }

  /** The live array of a folder; created on first touch. */
  dir(src: Src, path: string): Entry[] {
    const k = this.key(src, path);
    let list = this.dirs.get(k);
    if (!list) {
      const seed = isLocal(src) ? (LOCAL_WIN ? LOCAL_SEED : LINUX_LOCAL)[path] : REMOTE_SEED[path];
      if (path === '/srv/data/archive-10k' && !isLocal(src)) list = bigDir();
      else list = seed ? seed.map(e => ({ ...e })) : this.gen(src, path);
      this.dirs.set(k, list);
    }
    return list;
  }
  has(src: Src, path: string) { return this.dirs.has(this.key(src, path)); }
  set(src: Src, path: string, list: Entry[]) { this.dirs.set(this.key(src, path), list); }
  drop(src: Src) { for (const k of [...this.dirs.keys()]) if (k.startsWith(src + '|')) this.dirs.delete(k); }

  find(src: Src, path: string): Entry | undefined {
    const par = parentOf(src, path);
    if (!par) return undefined;
    const name = baseName(src, path);
    return this.dir(src, par).find(e => (isLocal(src) && LOCAL_WIN ? e.name.toLowerCase() === name.toLowerCase() : e.name === name));
  }

  canEnter(src: Src, user: string, e: Entry): boolean {
    if (isLocal(src) || user === 'root') return true;
    return e.owner === user ? e.perm[2] === 'x' : e.perm[8] === 'x';
  }
  canWrite(src: Src, user: string, path: string): boolean {
    if (isLocal(src) || user === 'root') return true;
    const e = this.find(src, path);
    if (!e) return false;
    if (e.owner === user) return e.perm[1] === 'w';
    if (e.group === user) return e.perm[4] === 'w';
    return e.perm[7] === 'w';
  }
  canRead(src: Src, user: string, e: Entry): boolean {
    if (isLocal(src) || user === 'root') return true;
    if (e.owner === user) return e.perm[0] === 'r';
    if (e.group === user) return e.perm[3] === 'r';
    return e.perm[6] === 'r';
  }
  /** Re-keys a folder and everything loaded under it after a rename or a move. */
  moveTree(src: Src, from: string, to: string, toSrc: Src = src) {
    const sep = LOCAL_WIN && isLocal(src) ? '\\' : '/';
    for (const [k, list] of [...this.dirs]) {
      const [s, path] = [k.slice(0, k.indexOf('|')), k.slice(k.indexOf('|') + 1)];
      if (s !== src || (path !== from && !path.startsWith(from + sep))) continue;
      this.dirs.delete(k);
      this.dirs.set(this.key(toSrc, to + path.slice(from.length)), list);
    }
  }
  /** Copies a loaded subtree to another place, adjusting ownership for the target side. */
  copyTree(src: Src, from: string, toSrc: Src, to: string, owner: string) {
    const sep = LOCAL_WIN && isLocal(src) ? '\\' : '/', tsep = LOCAL_WIN && isLocal(toSrc) ? '\\' : '/';
    const remote = !isLocal(toSrc);
    for (const [k, list] of [...this.dirs]) {
      const [s, path] = [k.slice(0, k.indexOf('|')), k.slice(k.indexOf('|') + 1)];
      if (s !== src || (path !== from && !path.startsWith(from + sep))) continue;
      const rest = path.slice(from.length).split(sep).join(tsep);
      this.dirs.set(this.key(toSrc, to + rest), list.map(e => ({
        ...e, owner: remote ? owner : '', group: remote ? owner : '',
        perm: remote ? e.perm || (e.dir ? 'rwxr-xr-x' : 'rw-r--r--') : '',
      })));
    }
  }
  ensureHome(src: Src, user: string) {
    if (user === 'root') return;
    const list = this.dir(src, '/home');
    if (!list.some(e => e.name === user)) list.push(rd(user, 0, user));
  }

  resolve(src: Src, user: string, raw: string, base?: string): string {
    let p = String(raw).trim();
    if (!p) throw fail('EINVAL', tr('Пустой путь'));
    let root: string, parts: string[];
    if (isLocal(src) && LOCAL_WIN) {
      p = p.replace(/\//g, '\\');
      let m = p.match(/^([a-zA-Z]):(\\.*)?$/);
      if (!m && base) { p = base.replace(/\\$/, '') + '\\' + p; m = p.match(/^([a-zA-Z]):(\\.*)?$/); }
      if (!m) throw fail('EINVAL', tr('Нужен путь вида D:\\папка'));
      const drive = m[1].toUpperCase();
      if (drive !== 'C' && drive !== 'D') throw fail('ENOENT', tr('Диска {d}: нет', { d: drive }));
      root = drive + ':\\';
      parts = (m[2] || '').split('\\');
    } else {
      const home = isLocal(src) ? '/home/user' : homeOf(user);
      if (p === '~' || p.startsWith('~/')) p = home + p.slice(1);
      else if (!p.startsWith('/')) {
        if (!base) throw fail('EINVAL', tr('Путь начинается с / или ~'));
        p = base.replace(/\/$/, '') + '/' + p;
      }
      root = '/';
      parts = p.split('/');
    }
    const norm: string[] = [];
    for (const part of parts) {
      if (!part || part === '.') continue;
      if (part === '..') norm.pop(); else norm.push(part);
    }
    let cur = root;
    for (const part of norm) {
      const list = this.dir(src, cur);
      const e = isLocal(src) && LOCAL_WIN ? list.find(x => x.name.toLowerCase() === part.toLowerCase()) : list.find(x => x.name === part);
      const next = childOf(src, cur, e ? e.name : part);
      if (!e) throw fail('ENOENT', tr('Нет такой папки: {p}', { p: next }));
      if (!e.dir) throw fail('ENOTDIR', tr('Это файл: {p}', { p: next }));
      if (!this.canEnter(src, user, e)) throw fail('EACCES', tr('Нет доступа для {u}: {p}', { u: user, p: next }));
      cur = next;
    }
    return cur;
  }
}
