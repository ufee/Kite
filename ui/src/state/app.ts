/* Start-up: subscriptions first, then the state the core already holds, then the last session or the first panes. */
import { watch } from 'vue';
import { backend, type HostKeyRequest, type PassphraseRequest } from '../backend';
import { settings, ui } from './prefs';
import { ask } from './overlay';
import { logPush, logUpdate, log } from './log';
import { initServers, loadServers, sessions } from './servers';
import { initPanes, panes, setSource } from './panes';
import { initQueue } from './queue';
import { initTerms } from './terms';
import { restoreLog, restoreSession, startPersist, type SessionFile } from './persist';

function initHostkeys() {
  backend.onHostkey((req: HostKeyRequest) => {
    ask<string>('hostkey', { ...req }).then(r => backend.hostkeyRespond(req.reqId, r === 'trust'));
  });
  // The dialog answers the phrase itself; closing it any other way cancels the key.
  backend.onPassphrase((req: PassphraseRequest) => {
    const name = req.file.split(/[\\/]/).pop() || req.file;
    ask<string>('passphrase', { ...req, name, phrase: '', add: req.agent, err: '', busy: false }).then(r => {
      if (r !== 'ok') backend.passphraseRespond(req.reqId, null, false).catch(() => undefined);
    });
  });
}

function syncSettings() {
  watch(() => settings.strictHost, v => backend.setStrictHost(v), { immediate: true });
  watch(() => settings.parallel, v => backend.transferSetParallel(v), { immediate: true });
  watch(() => settings.keepTime, v => backend.transferKeepTime(v), { immediate: true });
}

/** The very first launch: the mock opens its demo workspace, the real core two local panes. */
async function firstPanes(demo: boolean) {
  await Promise.all([
    setSource(0, 'local').then(() => {
      if (!demo || !panes[0].list.some(e => e.name === 'site.zip')) return;
      Object.assign(panes[0], { sel: ['site.zip'], cur: 'site.zip', anchor: 'site.zip' });
    }),
    demo ? setSource(1, 'demo-01', 'root').then(() => { panes[1].cur = 'app'; }) : setSource(1, 'local'),
  ]);
  if (demo) ui.focus = 1; // the server pane is the one in use
}

export async function initApp(session: SessionFile | null) {
  backend.onLogRow(logPush);
  backend.onLogUpdate(logUpdate);
  initServers();
  initHostkeys();
  initPanes();
  syncSettings();
  await loadServers();
  const demo = backend.kind === 'mock' && !session;
  if (demo) backend.seedDemo?.();
  const [tail, live] = await Promise.all([backend.logTail(), backend.sshSessions()]);
  const have = new Set(log.map(r => r.id));
  log.unshift(...tail.filter(r => !have.has(r.id))); // rows that came as events meanwhile are newer
  if (session) restoreLog(session);
  for (const s of live) sessions[s.server + '|' + s.user] = s.state;
  await initQueue();
  if (session) {
    await initTerms(false);
    await restoreSession(session);
  } else {
    await firstPanes(demo);
    await initTerms(demo); // the demo terminals open next to the demo panes
  }
  startPersist();
}
