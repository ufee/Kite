/*
 * A pretend shell behind a mock PTY. It speaks plain ANSI, so the terminal in the UI
 * is the same xterm.js that will later talk to a real PTY: line editing, history, Tab,
 * bracketed paste and OSC 7 for the current folder all happen here, not in the UI.
 */
import { collator, pad } from '../../util/format';
import { homeOf, LOCAL_WIN } from '../../util/paths';
import { tr } from '../../i18n';
import { DOCKER, LOCAL_PROJECT_WIN } from './data';
import type { MockFs } from './fs';
import type { PtyExit, Src } from '../types';

const C = {
  r: '\x1b[0m', bold: '\x1b[1m', g: '\x1b[32m', gb: '\x1b[1;32m', b: '\x1b[34m', bb: '\x1b[1;34m',
  y: '\x1b[33m', red: '\x1b[31m', m: '\x1b[90m',
};
const NL = '\r\n';

interface Ctx {
  src: Src;
  user: string;
  code: string;
  cwd: string;
}

export interface ShellHost {
  fs: MockFs;
  out(data: string): void;
  exit(reason: PtyExit): void;
  /** Resolves `ssh name` typed in the local shell. */
  server(name: string): { src: Src; code: string; defaultUser: string } | null;
  later(ms: number, fn: () => void): void;
}

export class MockShell {
  private stack: Ctx[];
  private buf = '';
  private pos = 0;
  private hist: string[] = [];
  private hIdx = -1;
  private busy = false;
  private pasting = false;
  private esc = '';

  constructor(private host: ShellHost, ctx: Ctx) {
    this.stack = [ctx];
  }

  private get ctx() { return this.stack[this.stack.length - 1]; }
  private get local() { return this.ctx.src === 'local'; }
  private get ps() { return this.local && LOCAL_WIN; }

  start(banner: boolean) {
    // Bracketed paste on: a pasted multi-line text must not run by itself.
    this.host.out('\x1b[?2004h');
    if (banner) this.banner();
    this.prompt();
  }

  private banner() {
    if (this.ps) this.host.out(`${C.m}PowerShell 7.4.6${C.r}${NL}${NL}`);
    else if (!this.local) this.host.out(`Welcome to Ubuntu 24.04.1 LTS (GNU/Linux 6.8.0-48-generic x86_64)${NL}${NL}${C.m}Last login: Fri Nov  6 18:04:11 2026 from 198.51.100.200${C.r}${NL}`);
  }

  private promptText(): string {
    const c = this.ctx;
    if (this.ps) return `${C.m}PS ${C.r}${c.cwd}> `;
    const home = this.local ? '/home/user' : homeOf(c.user);
    const short = c.cwd === home ? '~' : c.cwd.startsWith(home + '/') ? '~' + c.cwd.slice(home.length) : c.cwd;
    const who = this.local ? 'user@workstation' : `${c.user}@${c.code}`;
    return `${C.gb}${who}${C.r}:${C.bb}${short}${C.r}${c.user === 'root' ? '# ' : '$ '}`;
  }

  private osc7(): string {
    const c = this.ctx;
    const path = this.ps ? '/' + c.cwd.replace(/\\/g, '/') : c.cwd;
    return `\x1b]7;file://${this.local ? 'localhost' : c.code}${encodeURI(path)}\x07`;
  }

  private prompt() {
    this.buf = ''; this.pos = 0;
    this.host.out(this.osc7() + this.promptText());
  }

  private redraw() {
    const back = this.buf.length - this.pos;
    this.host.out('\r\x1b[K' + this.promptText() + this.buf + (back ? `\x1b[${back}D` : ''));
  }

  write(data: string) {
    if (this.busy) {
      if (data.includes('\x03')) this.host.out('^C' + NL);
      return;
    }
    let i = 0;
    while (i < data.length) {
      if (data.startsWith('\x1b[200~', i)) { this.pasting = true; i += 6; continue; }
      if (data.startsWith('\x1b[201~', i)) { this.pasting = false; i += 6; this.redraw(); continue; }
      const ch = data[i];
      if (this.pasting) {
        // The real shell keeps pasted line breaks in the edit buffer; this one has one line.
        this.insert(ch === '\r' || ch === '\n' ? ' ' : ch, false);
        i++;
        continue;
      }
      if (this.esc || ch === '\x1b') {
        this.esc += ch;
        i++;
        if (/^\x1b\[[0-9;]*[A-Za-z~]$/.test(this.esc) || /^\x1bO[A-Za-z]$/.test(this.esc)) { this.key(this.esc); this.esc = ''; }
        else if (this.esc.length > 8) this.esc = '';
        continue;
      }
      i++;
      switch (ch) {
        case '\r': this.enter(); break;
        case '\x7f': case '\b':
          if (this.pos > 0) { this.buf = this.buf.slice(0, this.pos - 1) + this.buf.slice(this.pos); this.pos--; this.redraw(); }
          break;
        case '\t': this.complete(); break;
        case '\x03': this.host.out('^C' + NL); this.prompt(); break;
        case '\x0c': this.host.out('\x1b[2J\x1b[3J\x1b[H'); this.redraw(); break;
        case '\x01': this.pos = 0; this.redraw(); break;
        case '\x05': this.pos = this.buf.length; this.redraw(); break;
        case '\x15': this.buf = this.buf.slice(this.pos); this.pos = 0; this.redraw(); break;
        case '\x04': if (!this.buf) this.run('exit'); break;
        default: if (ch >= ' ') this.insert(ch, true);
      }
    }
  }

  private insert(s: string, draw: boolean) {
    this.buf = this.buf.slice(0, this.pos) + s + this.buf.slice(this.pos);
    this.pos += s.length;
    if (!draw) return;
    if (this.pos === this.buf.length) this.host.out(s); else this.redraw();
  }

  private key(seq: string) {
    switch (seq.slice(-1)) {
      case 'A':
        if (this.hist.length) { this.hIdx = this.hIdx < 0 ? this.hist.length - 1 : Math.max(0, this.hIdx - 1); this.buf = this.hist[this.hIdx]; this.pos = this.buf.length; this.redraw(); }
        break;
      case 'B':
        if (this.hIdx >= 0) { this.hIdx++; this.buf = this.hIdx >= this.hist.length ? '' : this.hist[this.hIdx]; if (this.hIdx >= this.hist.length) this.hIdx = -1; this.pos = this.buf.length; this.redraw(); }
        break;
      case 'C': if (this.pos < this.buf.length) { this.pos++; this.host.out('\x1b[C'); } break;
      case 'D': if (this.pos > 0) { this.pos--; this.host.out('\x1b[D'); } break;
      case 'H': this.pos = 0; this.redraw(); break;
      case 'F': this.pos = this.buf.length; this.redraw(); break;
      case '~': if (seq === '\x1b[3~' && this.pos < this.buf.length) { this.buf = this.buf.slice(0, this.pos) + this.buf.slice(this.pos + 1); this.redraw(); } break;
    }
  }

  private complete() {
    const m = this.buf.slice(0, this.pos).match(/(\S*)$/), prefix = m ? m[1] : '';
    if (/[\\/]/.test(prefix)) return;
    const hits = this.host.fs.dir(this.ctx.src, this.ctx.cwd).filter(e => e.name.toLowerCase().startsWith(prefix.toLowerCase()));
    if (hits.length === 1) {
      const add = hits[0].name.slice(prefix.length) + (hits[0].dir ? (this.ps ? '\\' : '/') : ' ');
      this.insert(add, true);
    } else if (hits.length > 1) {
      this.host.out(NL + hits.map(e => (e.dir ? C.bb + e.name + C.r : e.name)).join('  ') + NL);
      this.redraw();
    }
  }

  private enter() {
    const line = this.buf;
    this.host.out(NL);
    this.hIdx = -1;
    if (line.trim()) { this.hist.push(line.trim()); if (this.hist.length > 500) this.hist.shift(); }
    if (!this.run(line.trim())) this.prompt();
  }

  private println(...lines: string[]) { for (const l of lines) this.host.out(l + NL); }

  /** Returns true when the command prints the next prompt itself. */
  private run(cmd: string): boolean {
    if (!cmd) return false;
    return this.ps ? this.runPs(cmd) : this.runSh(cmd);
  }

  private lsDate(ms: number) {
    const d = new Date(ms), MON = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
    return `${MON[d.getMonth()]} ${String(d.getDate()).padStart(2)} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  }

  private runSh(cmd: string): boolean {
    const c = this.ctx, fs = this.host.fs;
    const [name, ...args] = cmd.split(/\s+/);
    const flags = args.filter(a => a.startsWith('-')).join('');
    switch (name) {
      case 'help': this.println(C.m + tr('Мок понимает: ls, ll, cd, pwd, whoami, hostname, uptime, df -h, free -h, docker ps, systemctl status nginx, sudo -i, clear, exit') + C.r); break;
      case 'ls': case 'll': {
        const long = name === 'll' || flags.includes('l'), all = name === 'll' || flags.includes('a');
        const list = fs.dir(c.src, c.cwd).filter(e => all || !e.name.startsWith('.')).sort((a, b) => collator.compare(a.name, b.name));
        const col = (e: { dir: boolean; perm: string; name: string }) => (e.dir ? C.bb + e.name + C.r : e.perm[2] === 'x' ? C.gb + e.name + C.r : e.name);
        if (long) {
          this.println(`total ${list.length * 4}`);
          for (const e of list.slice(0, 2000)) this.println(`${e.dir ? 'd' : '-'}${e.perm || 'rw-r--r--'} 1 ${(e.owner || c.user).padEnd(6)} ${(e.group || c.user).padEnd(8)} ${String(e.dir ? 4096 : e.size).padStart(10)} ${this.lsDate(e.mtime)} ${col(e)}`);
        } else this.println(list.slice(0, 2000).map(col).join('  '));
        break;
      }
      case 'cd': {
        try { c.cwd = fs.resolve(c.src, c.user, args[0] || '~', c.cwd); } catch (e) {
          const code = (e as { code: string }).code;
          this.println(`-bash: cd: ${args[0]}: ${code === 'EACCES' ? 'Permission denied' : code === 'ENOTDIR' ? 'Not a directory' : 'No such file or directory'}`);
        }
        break;
      }
      case 'pwd': this.println(c.cwd); break;
      case 'whoami': this.println(c.user); break;
      case 'hostname': this.println(this.local ? 'workstation' : c.code); break;
      case 'uptime': this.println(' 14:32:07 up 41 days,  3:12,  1 user,  load average: 0.42, 0.37, 0.31'); break;
      case 'df': this.println('Filesystem      Size  Used Avail Use% Mounted on', '/dev/vda1        79G   41G   35G  55% /', '/dev/vdb1       492G  318G  149G  69% /home', 'tmpfs           3.9G     0  3.9G   0% /dev/shm'); break;
      case 'free': this.println('               total        used        free      shared  buff/cache   available', 'Mem:            15Gi       6.2Gi       1.1Gi       312Mi       8.1Gi       8.8Gi', 'Swap:          2.0Gi       128Mi       1.9Gi'); break;
      case 'docker': {
        if (args[0] !== 'ps') { this.println(tr('docker: в моке есть только docker ps')); break; }
        const rows = DOCKER[c.code] || [];
        this.println('CONTAINER ID   IMAGE                 STATUS        NAMES');
        rows.forEach((n, k) => this.println(`${(n.length * 7919 + k).toString(16).padStart(12, 'a')}   ${(n + ':latest').padEnd(20)}  Up ${3 + k} days    ${n}`));
        break;
      }
      case 'systemctl':
        this.println(`${C.g}● ${C.r}nginx.service - A high performance web server`, '     Loaded: loaded (/usr/lib/systemd/system/nginx.service; enabled)', `     Active: ${C.gb}active (running)${C.r} since Mon 2026-09-27 11:20:04 UTC; 1 month 10 days ago`);
        break;
      case 'sudo':
        if (args[0] === '-i' || args[0] === 'su') { c.user = 'root'; c.cwd = '/root'; } else this.println(C.m + tr('sudo: в моке есть только sudo -i') + C.r);
        break;
      case 'clear': this.host.out('\x1b[2J\x1b[3J\x1b[H'); break;
      case 'exit': case 'logout':
        this.println('logout');
        return this.leave();
      case 'vim': case 'nano': case 'mc': case 'htop': case 'top':
        this.println(C.m + tr('{c}: полноэкранная программа, в моке не открывается', { c: name }) + C.r);
        break;
      default: this.println(`-bash: ${name}: command not found`);
    }
    return false;
  }

  private runPs(cmd: string): boolean {
    const c = this.ctx, fs = this.host.fs;
    const [c0, ...args] = cmd.split(/\s+/), name = c0.toLowerCase();
    switch (name) {
      case 'help': this.println(C.m + tr('Мок понимает: ls, dir, cd, pwd, cls, whoami, git status, npm run build, ssh <сервер>, exit') + C.r); break;
      case 'ls': case 'dir': case 'gci': case 'get-childitem': {
        const list = fs.dir('local', c.cwd).sort((a, b) => (+b.dir - +a.dir) || collator.compare(a.name, b.name));
        const d = (ms: number) => { const x = new Date(ms); return `${pad(x.getDate())}.${pad(x.getMonth() + 1)}.${x.getFullYear()}    ${pad(x.getHours())}:${pad(x.getMinutes())}`; };
        this.println('', `    Directory: ${c.cwd}`, '', `${C.y}Mode                 LastWriteTime         Length Name${C.r}`, `${C.y}----                 -------------         ------ ----${C.r}`);
        for (const e of list) this.println(`${e.dir ? 'd----' : '-a---'}          ${d(e.mtime)} ${(e.dir ? '' : String(e.size)).padStart(14)} ${e.dir ? C.b + e.name + C.r : e.name}`);
        this.println('');
        break;
      }
      case 'cd': case 'set-location': case 'sl': {
        const arg = args.join(' ') || 'D:\\';
        try { c.cwd = fs.resolve('local', '', arg, c.cwd); } catch { this.println(`${C.red}Set-Location: Cannot find path '${arg}' because it does not exist.${C.r}`); }
        break;
      }
      case 'pwd': this.println('', 'Path', '----', c.cwd, ''); break;
      case 'cls': case 'clear': this.host.out('\x1b[2J\x1b[3J\x1b[H'); break;
      case 'whoami': this.println('workstation\\user'); break;
      case 'git':
        if (args[0] === 'status') this.println('On branch main', "Your branch is up to date with 'origin/main'.", '', 'nothing to commit, working tree clean');
        else this.println(C.m + tr('git: в моке есть только git status') + C.r);
        break;
      case 'npm':
        if (args.join(' ') !== 'run build') { this.println(C.m + tr('npm: в моке есть только npm run build') + C.r); break; }
        this.seq([
          [80, ''], [0, '> website@2.14.0 build'], [0, '> vite build'], [0, ''],
          [350, `${C.b}vite v8.3.7 ${C.g}building for production...${C.r}`],
          [700, `${C.g}✓ ${C.r}128 modules transformed.`],
          [250, `${C.m}build/${C.r}index.html          ${C.m}  2.01 kB${C.r}`],
          [60, `${C.m}build/${C.r}app.css             ${C.m} 38.40 kB │ gzip:   7.92 kB${C.r}`],
          [60, `${C.m}build/${C.r}app.js              ${C.m}412.18 kB │ gzip: 121.40 kB${C.r}`],
          [200, `${C.g}✓ built in 1.84s${C.r}`],
        ], () => { for (const e of fs.dir('local', LOCAL_PROJECT_WIN + '\\build')) if (!e.dir) e.mtime = Date.now(); });
        return true;
      case 'ssh': {
        const m = (args[0] || '').match(/^(?:([\w.-]+)@)?([\w.-]+)$/);
        const s = m && this.host.server(m[2]);
        if (!m || !s) { this.println(`${C.red}ssh: Could not resolve hostname ${args[0] || ''}${C.r}`); break; }
        const user = m[1] || s.defaultUser;
        fs.ensureHome(s.src, user);
        this.stack.push({ src: s.src, user, code: s.code, cwd: homeOf(user) });
        this.banner();
        break;
      }
      case 'exit': return this.leave();
      default: this.println(`${C.red}${c0}: The term '${c0}' is not recognized as a name of a cmdlet, function, script file, or executable program.${C.r}`);
    }
    return false;
  }

  private leave(): boolean {
    if (this.stack.length > 1) {
      const was = this.stack.pop()!;
      this.println(`Connection to ${was.code} closed.`);
      return false;
    }
    this.host.out('\x1b[?2004l');
    this.host.later(150, () => this.host.exit('exit'));
    return true;
  }

  private seq(steps: [number, string][], done?: () => void) {
    this.busy = true;
    let t = 0;
    for (const [ms, line] of steps) { t += ms; this.host.later(t, () => this.println(line)); }
    this.host.later(t + 10, () => { this.busy = false; done?.(); this.prompt(); });
  }

  /** Current folder for "Terminal here" and restore. */
  get cwd() { return this.ctx.cwd; }
}
