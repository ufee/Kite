import { isTauri } from '@tauri-apps/api/core';
import type { Backend } from './types';
import { MockBackend } from './mock';
import { TauriBackend } from './tauri';
import { browserStore, tauriStore } from './store';

/*
 * In the Tauri window the Rust core answers what it already has and the mock the rest (see tauri.ts).
 * In a plain browser tab, used for UI development and the stand comparison, everything is the mock.
 */
export const backend: Backend = isTauri() ? new TauriBackend(tauriStore) : new MockBackend(browserStore());

export * from './types';
