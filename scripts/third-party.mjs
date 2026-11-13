// Builds THIRD_PARTY_LICENSES.txt from what actually ships: Rust crates linked into the Windows exe
// and npm packages bundled into the UI. Dev and build-only dependencies stay out.
// Usage: node scripts/third-party.mjs [out-file]
import { execFileSync } from 'node:child_process';
import { existsSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const out = resolve(process.argv[2] ?? join(root, 'THIRD_PARTY_LICENSES.txt'));
const LICENSE_FILE = /^(licen[cs]e|copying|notice)([-_.].*)?$/i;

/** License texts found in a package folder, in a stable order. */
function licenseTexts(dir) {
  return readdirSync(dir, { withFileTypes: true })
    .filter(e => e.isFile() && LICENSE_FILE.test(e.name) && !e.name.endsWith('.spdx'))
    .map(e => e.name)
    .sort()
    .map(name => readFileSync(join(dir, name), 'utf8').replace(/\r\n/g, '\n').trim());
}

function rustCrates() {
  const cargo = process.platform === 'win32' ? 'cargo.exe' : 'cargo';
  const meta = JSON.parse(execFileSync(cargo, [
    'metadata', '--format-version', '1', '--locked',
    '--filter-platform', 'x86_64-pc-windows-msvc',
    '--manifest-path', join(root, 'src-tauri', 'Cargo.toml'),
  ], { maxBuffer: 64 << 20 }).toString());
  const pkgs = new Map(meta.packages.map(p => [p.id, p]));
  const nodes = new Map(meta.resolve.nodes.map(n => [n.id, n]));
  const isMacro = p => p.targets.some(t => t.kind.includes('proc-macro'));
  // Walk normal (linked) edges only; proc macros run at compile time and are not shipped.
  const seen = new Set([meta.resolve.root]);
  const queue = [meta.resolve.root];
  while (queue.length) {
    for (const dep of nodes.get(queue.shift()).deps) {
      if (seen.has(dep.pkg) || !dep.dep_kinds.some(k => k.kind === null)) continue;
      if (isMacro(pkgs.get(dep.pkg))) continue;
      seen.add(dep.pkg);
      queue.push(dep.pkg);
    }
  }
  seen.delete(meta.resolve.root);
  return [...seen].map(id => pkgs.get(id)).map(p => ({
    name: p.name, version: p.version, license: p.license ?? (p.license_file ? 'see file' : 'unknown'),
    url: p.repository ?? '', texts: licenseTexts(dirname(p.manifest_path)),
  }));
}

function npmPackages() {
  /** Node's lookup: the package's own node_modules, then each parent's. */
  const find = (name, from) => {
    for (let dir = from; ; dir = dirname(dir)) {
      const p = join(dir, 'node_modules', name);
      if (existsSync(join(p, 'package.json'))) return p;
      if (dir === dirname(dir) || dir === dirname(root)) return null;
    }
  };
  const found = new Map();
  const visit = (deps, from) => {
    for (const name of Object.keys(deps ?? {})) {
      const dir = find(name, from);
      if (!dir) throw new Error(`npm package not installed: ${name} (run npm ci)`);
      if (found.has(dir)) continue;
      const pkg = JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8'));
      const repo = typeof pkg.repository === 'string' ? pkg.repository : pkg.repository?.url ?? '';
      found.set(dir, { name: pkg.name, version: pkg.version, license: pkg.license ?? 'unknown', url: repo, texts: licenseTexts(dir) });
      visit(pkg.dependencies, dir);
    }
  };
  const ui = join(root, 'ui');
  visit(JSON.parse(readFileSync(join(ui, 'package.json'), 'utf8')).dependencies, ui);
  return [...found.values()];
}

// Licenses whose text carries no copyright line: a package that ships no file of its own
// gets the text another package ships under the same single license.
const STANDARD = ['Apache-2.0', 'MPL-2.0'];

function fillStandard(all) {
  const standard = new Map();
  for (const id of STANDARD) {
    const counts = new Map();
    for (const p of all) if (p.license === id && p.texts.length === 1) counts.set(p.texts[0], (counts.get(p.texts[0]) ?? 0) + 1);
    const best = [...counts].sort((a, b) => b[1] - a[1])[0];
    if (best) standard.set(id, best[0]);
  }
  for (const p of all) {
    if (p.texts.length) continue;
    const id = STANDARD.find(id => p.license.split(/\s+(?:OR|AND)\s+|[()]/).includes(id));
    if (id && standard.has(id)) p.texts = [standard.get(id)];
    else p.note = 'no license file in the package, see the repository';
  }
}

function render(sections) {
  // Identical texts (the Apache license in hundreds of crates) are printed once and referenced.
  const texts = new Map();
  const ref = t => texts.get(t) ?? (texts.set(t, texts.size + 1), texts.size);
  const lines = ['Kite includes the following third-party software.', ''];
  for (const [title, list] of sections) {
    lines.push(`== ${title} ==`, '');
    for (const p of list.sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version))) {
      const refs = p.texts.map(ref);
      lines.push(`${p.name} ${p.version}: ${p.license}${p.url ? ` <${p.url}>` : ''}${refs.length ? `, text ${refs.map(n => `[${n}]`).join(' ')}` : ''}${p.note ? ` (${p.note})` : ''}`);
    }
    lines.push('');
  }
  lines.push('== License texts ==', '');
  for (const [text, n] of texts) lines.push(`[${n}]`, '', text, '', '-'.repeat(72), '');
  return lines.join('\n');
}

const rust = rustCrates(), npm = npmPackages();
fillStandard([...rust, ...npm]);
writeFileSync(out, render([['Rust crates', rust], ['npm packages', npm]]));
console.log(`${out}: ${rust.length} crates, ${npm.length} npm packages`);
