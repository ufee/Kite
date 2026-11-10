/*
 * Where the files of the app data folder live. In the Tauri window the core writes them atomically
 * (servers.json, settings.json, session.json, terms/<id>.ansi); in a plain browser tab, used for
 * UI development and the stand comparison, they go to localStorage. `?reset` clears that copy.
 */
import { invoke } from '@tauri-apps/api/core';
import type { ServerConfig, StateDoc } from './types';

export interface Store {
  load(doc: StateDoc): Promise<unknown | null>;
  save(doc: StateDoc, data: unknown): Promise<void>;
  serversLoad(): Promise<ServerConfig | null>;
  serversSave(cfg: ServerConfig): Promise<void>;
  termLoad(id: number): Promise<string | null>;
  termSave(id: number, data: string): Promise<void>;
  termPrune(keep: number[]): Promise<void>;
}

export const tauriStore: Store = {
  load: doc => invoke('state_load', { doc }),
  save: (doc, data) => invoke('state_save', { doc, data }),
  serversLoad: () => invoke('servers_load'),
  serversSave: cfg => invoke('servers_save', { cfg }),
  termLoad: id => invoke('term_load', { id }),
  termSave: (id, data) => invoke('term_save', { id, data }),
  termPrune: keep => invoke('term_prune', { keep }),
};

const KEY = 'kite.';
const get = <T>(k: string): T | null => {
  try { const v = localStorage.getItem(KEY + k); return v == null ? null : (JSON.parse(v) as T); } catch { return null; }
};
const set = (k: string, v: unknown) => {
  try { localStorage.setItem(KEY + k, JSON.stringify(v)); } catch { /* storage may be blocked or full */ }
};
const del = (k: string) => { try { localStorage.removeItem(KEY + k); } catch { /* blocked */ } };

export function browserStore(): Store {
  if (new URLSearchParams(location.search).has('reset')) {
    try { for (const k of Object.keys(localStorage)) if (k.startsWith(KEY)) localStorage.removeItem(k); } catch { /* blocked */ }
    history.replaceState(null, '', location.pathname);
  }
  return {
    load: async doc => get(doc),
    save: async (doc, data) => set(doc, data),
    serversLoad: async () => get<ServerConfig>('servers'),
    serversSave: async cfg => set('servers', cfg),
    termLoad: async id => get<string>('term.' + id),
    termSave: async (id, data) => set('term.' + id, data),
    termPrune: async keep => {
      try {
        for (const k of Object.keys(localStorage)) {
          const m = k.match(/^kite\.term\.(\d+)$/);
          if (m && !keep.includes(+m[1])) del(k.slice(KEY.length));
        }
      } catch { /* blocked */ }
    },
  };
}
