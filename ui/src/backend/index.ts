import type { Backend } from './types';
import { MockBackend } from './mock';

/**
 * Stage 1: the mock runs everywhere, in the browser and inside the Tauri window.
 * The Rust core takes over method by method in later stages behind the same interface.
 */
export const backend: Backend = new MockBackend();

export * from './types';
