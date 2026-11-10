import { readFileSync } from 'node:fs';
import { execSync } from 'node:child_process';
import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';

// One version for the app and the UI: tauri.conf.json is the source.
const conf = JSON.parse(readFileSync(new URL('../src-tauri/tauri.conf.json', import.meta.url), 'utf8')) as { version: string };
let commit = '';
try { commit = execSync('git rev-parse --short HEAD', { stdio: ['ignore', 'pipe', 'ignore'] }).toString().trim(); } catch { /* not a git checkout */ }

// Tauri expects a fixed port and no clearing of its own output.
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: { port: 5174, strictPort: true },
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  define: {
    __APP_VERSION__: JSON.stringify(conf.version),
    __APP_COMMIT__: JSON.stringify(commit),
  },
  build: {
    target: 'es2023',
    sourcemap: false,
    chunkSizeWarningLimit: 1500,
  },
});
