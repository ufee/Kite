import { isTauri } from '@tauri-apps/api/core';
import type { Backend } from './types';
import { MockBackend } from './mock';
import { browserStore, tauriStore } from './store';

/*
 * The mock still runs everything except storage: in the Tauri window the core already keeps the files,
 * in a browser tab they live in localStorage. The Rust core takes over method by method in later stages
 * behind the same interface.
 */
export const backend: Backend = new MockBackend(isTauri() ? tauriStore : browserStore());

export * from './types';
