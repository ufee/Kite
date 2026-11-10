import { createApp } from 'vue';
import '@fontsource-variable/inter';
import '@fontsource-variable/jetbrains-mono';
import '@xterm/xterm/css/xterm.css';
import './styles/app.css';
import App from './App.vue';
import { guardWebviewKeys } from './platform/keys';
import { initWindow } from './platform/window';
import { initApp } from './state/app';

guardWebviewKeys();
createApp(App).mount('#app');
initApp().catch(e => console.error('init failed', e));
initWindow();
