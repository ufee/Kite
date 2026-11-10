/* Start-up: subscriptions first, then the state the core already holds, then the first panes. */
import { watch } from 'vue';
import { backend, type HostKeyRequest } from '../backend';
import { settings, ui } from './prefs';
import { ask } from './overlay';
import { logPush, logUpdate, log } from './log';
import { initServers, loadServers, sessions } from './servers';
import { initPanes, panes, setSource } from './panes';
import { initQueue } from './queue';
import { initTerms } from './terms';

function initHostkeys() {
  backend.onHostkey((req: HostKeyRequest) => {
    ask<string>('hostkey', { ...req }).then(r => backend.hostkeyRespond(req.reqId, r === 'trust'));
  });
}

function syncSettings() {
  watch(() => settings.strictHost, v => backend.setStrictHost(v), { immediate: true });
  watch(() => settings.parallel, v => backend.transferSetParallel(v), { immediate: true });
  watch(() => settings.keepTime, v => backend.transferKeepTime(v), { immediate: true });
}

export async function initApp() {
  backend.onLogRow(logPush);
  backend.onLogUpdate(logUpdate);
  initServers();
  initHostkeys();
  initPanes();
  syncSettings();
  await loadServers();
  const [tail, live] = await Promise.all([backend.logTail(), backend.sshSessions()]);
  const have = new Set(log.map(r => r.id));
  log.unshift(...tail.filter(r => !have.has(r.id))); // rows that came as events meanwhile are newer
  for (const s of live) sessions[s.server + '|' + s.user] = s.state;
  await initQueue();

  // Stage 1 always runs on the mock: open the demo workspace.
  const demo = backend.kind === 'mock';
  await Promise.all([
    setSource(0, 'local').then(() => {
      if (!demo || !panes[0].list.some(e => e.name === 'site.zip')) return;
      Object.assign(panes[0], { sel: ['site.zip'], cur: 'site.zip', anchor: 'site.zip' });
    }),
    demo ? setSource(1, 'demo-01', 'root').then(() => { panes[1].cur = 'app'; }) : setSource(1, 'local'),
  ]);
  if (demo) ui.focus = 1; // the server pane is the one in use
  await initTerms(demo);
}
