/* Neutral demo data for the mock: made-up servers, documentation IP ranges, generic projects. */
import type { Entry, Server } from '../types';
import { DAY, GB, HOUR, KB, MB, MIN } from '../../util/format';

export const NOW = Date.now();

const ent = (name: string, dir: boolean, size: number, ago: number, perm = '', owner = '', group = owner): Entry =>
  ({ name, dir, size: Math.round(size), mtime: NOW - Math.round(ago), perm, owner, group });
export const ld = (name: string, ago = 3 * DAY) => ent(name, true, 0, ago);
export const lf = (name: string, size: number, ago = DAY) => ent(name, false, size, ago);
export const rd = (name: string, ago: number, o = 'root', g = o, perm = 'rwxr-xr-x') => ent(name, true, 0, ago, perm, o, g);
export const rf = (name: string, size: number, ago: number, o = 'root', g = o, perm = 'rw-r--r--') => ent(name, false, size, ago, perm, o, g);

export const LOCAL_HOME_WIN = 'D:\\Projects\\website\\build';
export const LOCAL_PROJECT_WIN = 'D:\\Projects\\website';

export const LOCAL_SEED: Record<string, Entry[]> = {
  'C:\\': [ld('Program Files', 30 * DAY), ld('Users', 90 * DAY), ld('Windows', 5 * DAY)],
  'D:\\': [ld('Backups', 20 * MIN), ld('Dev', 2 * DAY), ld('Downloads', 3 * HOUR), ld('Projects', HOUR)],
  'D:\\Backups': [lf('shop-2026-11-04.sql.gz', 1.78 * GB, 3 * DAY), lf('shop-2026-11-05.sql.gz', 1.79 * GB, 2 * DAY)],
  'D:\\Downloads': [lf('servers.json', 3.4 * KB, 3 * HOUR), lf('id_ed25519.pub', 98, 40 * DAY), lf('report-oct.xlsx', 86 * KB, 6 * DAY), { ...lf('desktop.ini', 282, 90 * DAY), hidden: true }],
  'D:\\Projects': [ld('blog', 9 * DAY), ld('dotfiles', 2 * DAY), ld('homelab', 5 * HOUR), ld('shop', 4 * DAY), ld('website', 40 * MIN)],
  'D:\\Projects\\website': [ld('build', 18 * MIN), ld('node_modules', 3 * DAY), ld('src', 40 * MIN), lf('.gitignore', 312, 60 * DAY), lf('package.json', 1.9 * KB, 3 * DAY), lf('README.md', 4.2 * KB, 8 * DAY), lf('vite.config.js', 1.1 * KB, 3 * DAY)],
  'D:\\Projects\\website\\build': [ld('assets', 18 * MIN), ld('locales', 18 * MIN), lf('app.css', 38 * KB, 18 * MIN), lf('app.js', 412 * KB, 18 * MIN), lf('index.html', 2 * KB, 18 * MIN), lf('manifest.json', 1.2 * KB, 18 * MIN), lf('site.zip', 2.4 * MB, 17 * MIN)],
};

export const REMOTE_SEED: Record<string, Entry[]> = {
  '/': [rd('bin', 90 * DAY), rd('boot', 40 * DAY), rd('etc', 2 * DAY), rd('home', 30 * DAY), rd('opt', 20 * DAY), rd('root', 2 * DAY, 'root', 'root', 'rwx------'), rd('srv', 90 * DAY), rd('tmp', 5 * MIN, 'root', 'root', 'rwxrwxrwt'), rd('usr', 40 * DAY), rd('var', DAY)],
  '/home': [rd('admin', DAY, 'admin'), rd('deploy', 3 * DAY, 'deploy')],
  '/home/admin': [rd('.ssh', 30 * DAY, 'admin', 'admin', 'rwx------'), rd('backups', 6 * HOUR, 'admin'), rd('web', 2 * DAY, 'admin'), rf('.bashrc', 3.7 * KB, 200 * DAY, 'admin'), rf('.profile', 807, 200 * DAY, 'admin')],
  '/home/admin/web': [rd('app', 2 * HOUR, 'admin')],
  '/home/admin/web/app': [rd('logs', 5 * MIN, 'admin'), rd('public_html', 2 * HOUR, 'admin'), rd('releases', 2 * HOUR, 'admin')],
  '/home/admin/web/app/public_html': [rd('app', 2 * HOUR, 'admin'), rd('config', 2 * HOUR, 'admin'), rd('public', 2 * HOUR, 'admin'), rd('storage', 10 * MIN, 'admin', 'www-data', 'rwxrwxr-x'), rd('vendor', 3 * DAY, 'admin'), rf('.env', 1.4 * KB, 9 * DAY, 'admin', 'admin', 'rw-------'), rf('composer.json', 2.1 * KB, 3 * DAY, 'admin'), rf('composer.lock', 284 * KB, 3 * DAY, 'admin'), rf('index.php', 1.2 * KB, 40 * DAY)],
  '/home/admin/backups': [rd('daily', 6 * HOUR, 'admin'), rd('weekly', 4 * DAY, 'admin'), rf('pg-2026-11-06.dump', 3.2 * GB, 9 * HOUR, 'admin'), rf('restore.sh', 2.3 * KB, 30 * DAY, 'admin', 'admin', 'rwxr-xr-x'), rf('shop-2026-11-05.sql.gz', 1.79 * GB, 33 * HOUR, 'admin'), rf('shop-2026-11-06.sql.gz', 1.8 * GB, 9 * HOUR, 'admin')],
  '/root': [rd('.ssh', 40 * DAY, 'root', 'root', 'rwx------'), rd('backups', DAY), rd('compose', 5 * DAY), rf('.bashrc', 3.1 * KB, 300 * DAY), rf('.profile', 161, 300 * DAY), rf('docker-compose.yml', 2.6 * KB, 5 * DAY)],
  '/home/deploy': [rd('.ssh', 30 * DAY, 'deploy', 'deploy', 'rwx------'), rd('releases', 2 * HOUR, 'deploy'), rf('.bashrc', 3.7 * KB, 200 * DAY, 'deploy'), rf('deploy.sh', 4.1 * KB, 6 * DAY, 'deploy', 'deploy', 'rwxr-xr-x')],
  '/srv': [rd('data', 3 * DAY)],
  '/srv/data': [rd('archive-10k', 3 * DAY), rf('README', 420, 30 * DAY)],
  '/var': [rd('cache', 2 * DAY), rd('lib', 10 * DAY), rd('log', 5 * MIN, 'root', 'syslog', 'rwxrwxr-x'), { ...rd('www', 40 * DAY), link: true }],
};

/** A folder of 10 000 files: the virtual list must scroll it without jank. */
export function bigDir(): Entry[] {
  const out: Entry[] = [];
  for (let k = 0; k < 10000; k++) out.push(rf(`part-${String(k).padStart(5, '0')}.bin`, 4 * KB + ((k * 7919) % 900) * KB, (k % 500) * HOUR));
  return out;
}

export const POOL = {
  rdir: ['cache', 'config', 'data', 'lib', 'logs', 'modules', 'releases', 'shared', 'src', 'tmp'],
  rfile: ['access.log', 'config.php', 'error.log', 'helpers.php', 'index.php', 'Kernel.php', 'README.md', 'routes.php', 'schema.sql', 'settings.json', '.htaccess'],
  ldir: ['assets', 'components', 'docs', 'images', 'scripts', 'styles', 'utils'],
  lfile: ['App.vue', 'index.ts', 'logo.svg', 'main.ts', 'notes.md', 'report.xlsx', 'styles.css'],
};

export type Kind = 'web' | 'db' | 'home';
export interface DemoServer extends Server {
  kind: Kind;
  offline?: boolean;
  hostKeyChanged?: boolean;
}

const USERS3 = () => [{ name: 'root', key: 'auto' }, { name: 'admin', key: 'id_ed25519' }, { name: 'deploy', key: 'deploy_ed25519' }];
let seq = 0;
const srv = (code: string, group: string, description: string, host: string, kind: Kind, extra: Partial<DemoServer> = {}): DemoServer => ({
  id: `demo-${String(++seq).padStart(2, '0')}`, code, group, description, host, port: 22, users: USERS3(), defaultUser: 'root',
  keyMode: 'auto', keyFile: '', authPass: false, startDir: '', keepalive: 30, kind, ...extra,
});

export const DEMO_SERVERS: DemoServer[] = [
  srv('web1', 'Production', 'Главный веб-сервер', '203.0.113.17', 'web'),
  srv('web2', 'Production', 'Второй веб-узел', '203.0.113.18', 'web'),
  srv('api1', 'Production', 'API и воркеры очередей', '203.0.113.19', 'home'),
  srv('db1', 'Production', 'MySQL и Redis', '203.0.113.20', 'db'),
  srv('db2', 'Production', 'PostgreSQL и бэкапы', '203.0.113.30', 'db'),
  srv('stage1', 'Staging', 'Тестовый веб', '198.51.100.3', 'web'),
  srv('stage2', 'Staging', 'Тестовый API, вебхуки', '198.51.100.7', 'web', { hostKeyChanged: true }),
  srv('ci1', 'Staging', 'Сборки CI', '198.51.100.21', 'home', { offline: true }),
  srv('nas', 'Дом', 'NAS и медиа', '192.0.2.10', 'home'),
  srv('pi', 'Дом', 'Home Assistant', '192.0.2.11', 'home'),
  srv('vpn', 'Дом', 'WireGuard', '192.0.2.21', 'home', { users: [{ name: 'root', key: 'auto' }] }),
  srv('lab', 'Дом', 'Песочница Docker', '192.0.2.22', 'home'),
];
export const DEMO_GROUPS = ['Production', 'Staging', 'Дом'];

/** Key names in ~/.ssh with a .pub pair. */
export const DEMO_KEYS = ['id_ed25519', 'deploy_ed25519', 'work_ed25519'];
export const DEMO_AGENT_KEYS = ['id_ed25519', 'deploy_ed25519'];

export const DOCKER: Record<string, string[]> = {
  web1: ['nginx', 'php-fpm'], web2: ['nginx', 'php-fpm'], api1: ['api', 'worker', 'redis'],
  nas: ['jellyfin', 'samba'], pi: ['homeassistant', 'mosquitto'], lab: ['postgres', 'redis', 'playground'],
};

/** Demo start folders; real servers start in the card's folder or at home. */
function demoStartDir(kind: Kind): string {
  return kind === 'web' ? '/home/admin/web/app/public_html' : kind === 'db' ? '/home/admin/backups' : '';
}
for (const s of DEMO_SERVERS) s.startDir = demoStartDir(s.kind);

export const SAMPLE_TEXT: Record<string, string> = {
  css: 'body {\n  margin: 0;\n  font-family: Inter, sans-serif;\n}\n',
  js: "export function mount(root) {\n  root.dataset.ready = 'true';\n}\n",
  html: '<!doctype html>\n<title>Website</title>\n<div id="app"></div>\n',
  json: '{\n  "name": "website",\n  "version": "1.4.2"\n}\n',
  php: "<?php\n\nreturn [\n    'name' => 'shop',\n    'debug' => false,\n];\n",
  env: 'APP_ENV=production\nAPP_DEBUG=false\n',
};
