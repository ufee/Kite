import type { Src } from '../backend/types';

/** Path flavor of "This PC". Windows paths use drive roots and backslashes. */
export const LOCAL_WIN = !/linux/i.test(navigator.userAgent);

export const isLocal = (s: Src): s is 'local' => s === 'local';
const winStyle = (s: Src) => isLocal(s) && LOCAL_WIN;
export const sepOf = (s: Src) => (winStyle(s) ? '\\' : '/');

export function splitPath(src: Src, path: string): { root: string; parts: string[] } {
  if (winStyle(src)) {
    const [d, ...rest] = path.split('\\');
    return { root: d + '\\', parts: rest.filter(Boolean) };
  }
  return { root: '/', parts: path.split('/').filter(Boolean) };
}
export const joinPath = (src: Src, root: string, parts: string[]) =>
  winStyle(src) ? root + parts.join('\\') : '/' + parts.join('/');

export function childOf(src: Src, path: string, name: string): string {
  if (isDrives(src, path)) return name.replace(/\\?$/, '\\'); // a drive in the list of drives: its root
  const { root, parts } = splitPath(src, path);
  return joinPath(src, root, [...parts, name]);
}
export function parentOf(src: Src, path: string): string | null {
  const { root, parts } = splitPath(src, path);
  return parts.length ? joinPath(src, root, parts.slice(0, -1)) : null;
}
export function baseName(src: Src, path: string): string {
  const { root, parts } = splitPath(src, path);
  return parts.length ? parts[parts.length - 1] : root;
}
/** "This PC" on Windows: the list of drives, above the drive roots. Its path is empty. */
export const isDrives = (src: Src, path: string) => winStyle(src) && path === '';
/** Where ".." leads in a pane: on Windows a drive root leads to the list of drives (''); null at the top. */
export function upOf(src: Src, path: string): string | null {
  if (isDrives(src, path)) return null;
  return parentOf(src, path) ?? (winStyle(src) ? '' : null);
}
/** True if `inner` is `outer` itself or lies inside it. */
export function within(src: Src, outer: string, inner: string): boolean {
  return inner === outer || inner.startsWith(outer.replace(/[\\/]$/, '') + sepOf(src));
}
/** The drive root on Windows, `/` elsewhere. */
export const rootOf = (src: Src, path: string) => splitPath(src, path).root;
export const homeOf = (user: string) => (user === 'root' ? '/root' : '/home/' + user);
