import { createApp } from 'vue';
import '@fontsource-variable/inter';
import '@fontsource-variable/jetbrains-mono';
import '@xterm/xterm/css/xterm.css';
import './styles/app.css';
import App from './App.vue';
import { guardWebviewKeys } from './platform/keys';
import { initWindow } from './platform/window';
import { initApp } from './state/app';
import { loadState } from './state/persist';

guardWebviewKeys();
// Settings first: the first frame already has the saved theme and layout.
loadState().then(session => {
  createApp(App).mount('#app');
  initWindow();
  return initApp(session);
}).catch(e => console.error('init failed', e));
