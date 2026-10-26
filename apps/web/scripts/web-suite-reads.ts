// WHAT THE WEB SUITE READS, DERIVED FROM ITS OWN FILES (backlog a244c6e7).
//
// infra/gate.sh runs the web app's build and mocked Playwright suite in
// --auto only when a car changes something they read. Until 2026-10-19
// "something they read" was a grep for apps/web, apps/simulator and
// libs/web-kit, while the specs also load the step-plugin bundles (17
// reads), the estate's dev-door declaration, and the bundle imports the
// architecture diagrams out of docs/. A car that changed only a step
// plugin was gated green without the specs that render it, and the
// conductor gates trains with --auto too, so the forge's web job was the
// first run they got. A second, hand-kept list of those paths would be
// a fact living twice (CLAUDE.md 9a): the next spec that reads a new
// file would drift from it in silence.
//
// So the answer is read off the suite itself, on every gate:
//
//   OWN TREES  apps/web and every workspace package it depends on
//              (`workspace:` in its package.json, found through the root
//              manifest's `workspaces`) — today libs/web-kit.
//   INSTALL    the root manifest, its lockfile, and each patch its
//              `patchedDependencies` names: what `bun install` reads.
//   ESCAPES    every relative literal (a quoted path, or a template's
//              head cut back to its directory) in the code, HTML and CSS
//              of those trees that resolves outside them: a `new URL(…,
//              import.meta.url)` a spec reads, an import the bundle
//              takes. Code, not prose: each file goes through Bun's
//              transpiler first, which drops comments and type-only
//              imports and keeps every literal (a .svelte file by its
//              <script> blocks).
//
// Unit-test files are left out — the gate runs every unit suite on every
// tree now, so what THEY read is judged regardless — and are named the
// way the runner names them: TEST_FILE_GLOB under the roots each
// package's own `test:unit` hands it.
//
// WHAT A SCAN CANNOT SEE is a path built from the file's own place or
// the cwd (`import.meta.dir`, `__dirname`, `process.cwd()`, a bare
// `import.meta.url`, a literal that reaches the repo root itself). A
// file doing that must say what it reads, in its own text:
//
//   // web-suite-reads: none — <why>
//   // web-suite-reads: infra/some/dir/ docs/x.md — <why>
//
// A file that does not is UNRESOLVED: the pin beside this file goes red
// naming it, and the gate, unable to judge, runs the suite rather than
// skip it. Silence is never read as "reads nothing".
//
// Usage (from apps/web; imports nothing but bun and node builtins, so it
// runs before any install):
//   bun scripts/web-suite-reads.ts             the derived reads, one per line
//   bun scripts/web-suite-reads.ts --touched   changed paths on stdin;
//                                              prints yes or no, and why on stderr
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';

import { TEST_FILE_GLOB } from './each-test-file-alone';

export type Derived = Readonly<{
  /** Repo-relative prefixes: a changed path equal to or under one is read. Sorted. */
  reads: ReadonlyArray<string>;
  /** One line per file whose reads the scan cannot see and that declares none. */
  unresolved: ReadonlyArray<string>;
  /** Which scanned files named each read, for the gate's log. */
  readers: Readonly<Record<string, ReadonlyArray<string>>>;
}>;

type Manifest = {
  name?: string;
  workspaces?: string[];
  dependencies?: Record<string, string>;
  devDependencies?: Record<string, string>;
  patchedDependencies?: Record<string, string>;
  scripts?: Record<string, string>;
};

const manifest = (dir: string): Manifest =>
  JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8')) as Manifest;

/// apps/web and the workspace packages it depends on, transitively, as
/// repo-relative directories.
function ownTrees(repo: string, app: string): string[] {
  const workspaces = (manifest(repo).workspaces ?? []).flatMap((pattern) =>
    [...new Bun.Glob(`${pattern}/package.json`).scanSync({ cwd: repo, onlyFiles: true })].map(dirname),
  );
  const byName = new Map(workspaces.map((dir) => [manifest(join(repo, dir)).name, dir] as const));
  const found = new Set<string>();
  const visit = (dir: string) => {
    if (found.has(dir)) return;
    found.add(dir);
    const m = manifest(join(repo, dir));
    for (const [name, spec] of Object.entries({ ...m.dependencies, ...m.devDependencies })) {
      const dep = byName.get(name);
      if (spec.startsWith('workspace:') && dep) visit(dep);
    }
  };
  visit(app);
  return [...found];
}

/// What `bun install` reads at the workspace root.
function installInputs(repo: string): string[] {
  const lockfiles = ['bun.lock', 'bun.lockb'].filter((f) => existsSync(join(repo, f)));
  return ['package.json', ...lockfiles, ...Object.values(manifest(repo).patchedDependencies ?? {})];
}

/// The roots a package's `test:unit` hands each-test-file-alone.ts.
function unitRoots(repo: string, tree: string): string[] {
  const script = manifest(join(repo, tree)).scripts?.['test:unit'] ?? '';
  const m = script.match(/^bun \S*each-test-file-alone\.ts((?: [\w/.-]+)+)$/);
  return m ? m[1]!.trim().split(' ') : [];
}

/// What the build and the specs load: code, and the HTML and CSS the
/// bundle takes. Never node_modules (the install's, judged by its
/// inputs above) or dist (the build's own output).
const LOADED = /\.(?:[cm]?[jt]sx?|svelte|html|css)$/;
const SKIP_DIRS = new Set(['node_modules', 'dist']);

/// Every file under `dir`, relative to it, without descending SKIP_DIRS.
function walk(dir: string, prefix = ''): string[] {
  return readdirSync(join(dir, prefix), { withFileTypes: true }).flatMap((e) => {
    const rel = prefix ? `${prefix}/${e.name}` : e.name;
    if (e.isDirectory()) return SKIP_DIRS.has(e.name) ? [] : walk(dir, rel);
    return e.isFile() ? [rel] : [];
  });
}

const transpilers = new Map<string, Bun.Transpiler>();
const transpiler = (loader: 'ts' | 'tsx' | 'js' | 'jsx'): Bun.Transpiler => {
  let t = transpilers.get(loader);
  if (!t) transpilers.set(loader, (t = new Bun.Transpiler({ loader })));
  return t;
};

/// A file's code with comments and type-only imports gone.
function code(path: string, text: string): string {
  if (path.endsWith('.svelte')) {
    return [...text.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/g)]
      .map((m) => transpiler('ts').transformSync(m[1]!))
      .join('\n');
  }
  const loader = /\.[cm]?tsx$/.test(path) ? 'tsx' : /\.[cm]?ts$/.test(path) ? 'ts' : /x$/.test(path) ? 'jsx' : 'js';
  return transpiler(loader).transformSync(text);
}

/// Every relative literal in transpiled code: a double-quoted string (the
/// transpiler's spelling of every quoted string), or a template's head
/// cut back to its directory — `../x/${n}.js` reads all of `../x/`.
function relativeLiterals(js: string): string[] {
  const quoted = [...js.matchAll(/"(\.{1,2}\/(?:[^"\\\n]|\\.)*)"/g)].map((m) => m[1]!);
  const templates = [...js.matchAll(/`(\.{1,2}\/[^`$]*)(\$\{|`)/g)].map((m) =>
    m[2] === '`' ? m[1]! : m[1]!.slice(0, m[1]!.lastIndexOf('/') + 1),
  );
  return [...quoted, ...templates];
}

/// The relative references in HTML and CSS: quoted, or inside `url()`.
function markupReferences(text: string): string[] {
  return [...text.matchAll(/["'(](\.{1,2}\/[^"')\s]*)/g)].map((m) => m[1]!);
}

/// The forms that build a path the scan cannot read.
const HIDDEN: ReadonlyArray<readonly [string, RegExp]> = [
  ['import.meta.dir/dirname/path/filename/resolve', /\bimport\.meta\.(?:dir|dirname|path|filename|resolve)\b/],
  ['__dirname/__filename', /\b__(?:dirname|filename)\b/],
  ['process.cwd()', /\bprocess\.cwd\s*\(/],
  ['Bun.main', /\bBun\.main\b/],
];
const LITERAL_URL = /new URL\(\s*(?:"(?:[^"\\\n]|\\.)*"|`[^`]*`)\s*,\s*import\.meta\.url\s*\)/g;

/// The `web-suite-reads:` declaration in a file's raw text, if any — a
/// comment line of its own — and the paths it names (none for `none`).
function declared(text: string): string[] | null {
  const m = text.match(/^\s*(?:\/\/|\*|#|<!--)\s*web-suite-reads:\s*(.*)$/m);
  if (!m) return null;
  const words = m[1]!.trim().split(/\s+/);
  if (words[0] === 'none') return [];
  const paths = [];
  for (const w of words) {
    if (!/^[\w@.%-][\w@./%-]*$/.test(w)) break;
    paths.push(w);
  }
  return paths.length > 0 ? paths : null;
}

export function webSuiteReads(repoRoot: string, app = 'apps/web'): Derived {
  const repo = resolve(repoRoot);
  const trees = ownTrees(repo, app);
  const own = trees.map((t) => `${t}/`);
  const inside = (rel: string) => own.some((t) => `${rel}/`.startsWith(t));
  const readers = new Map<string, Set<string>>();
  const add = (read: string, by: string) => {
    if (!readers.has(read)) readers.set(read, new Set());
    readers.get(read)!.add(by);
  };
  for (const t of own) add(t, 'the suite itself');
  for (const f of installInputs(repo)) add(f, 'bun install');
  const unresolved: string[] = [];

  const unitGlob = new Bun.Glob(TEST_FILE_GLOB);
  for (const tree of trees) {
    const units = unitRoots(repo, tree);
    const files = walk(join(repo, tree))
      .filter((f) => LOADED.test(f))
      .filter((f) => !(unitGlob.match(f) && units.some((root) => f.startsWith(`${root}/`))))
      .sort();
    for (const f of files) {
      const rel = `${tree}/${f}`;
      const text = readFileSync(join(repo, rel), 'utf8');
      const hidden: string[] = [];
      let literals: string[];
      if (/\.(?:html|css)$/.test(f)) {
        literals = markupReferences(text);
      } else {
        let js: string;
        try {
          js = code(rel, text);
        } catch (e) {
          unresolved.push(`${rel}: the transpiler cannot read it (${(e as Error).message.split('\n')[0]})`);
          continue;
        }
        hidden.push(...HIDDEN.filter(([, re]) => re.test(js)).map(([name]) => name));
        if (/\bimport\.meta\.url\b/.test(js.replace(LITERAL_URL, ''))) {
          hidden.push('import.meta.url outside new URL(<literal>, import.meta.url)');
        }
        literals = relativeLiterals(js);
      }
      for (const lit of literals) {
        const target = relative(repo, resolve(repo, dirname(rel), lit));
        if (target === '' || target.startsWith('..')) {
          hidden.push(`'${lit}' names the repository root or above it`);
          continue;
        }
        if (inside(target)) continue;
        add(lit.endsWith('/') ? `${target}/` : target, rel);
      }
      if (hidden.length === 0) continue;
      const says = declared(text);
      if (says === null) {
        unresolved.push(
          `${rel}: ${hidden.join('; ')} — builds a path this scan cannot read; ` +
            'say what it reads in a `web-suite-reads:` line (see apps/web/scripts/web-suite-reads.ts)',
        );
      } else {
        for (const p of says) add(p, rel);
      }
    }
  }
  const reads = [...readers.keys()].sort();
  return {
    reads,
    unresolved,
    readers: Object.fromEntries(reads.map((r) => [r, [...readers.get(r)!].sort()])),
  };
}

export type Answer = Readonly<{ touched: boolean; why: string }>;

/// Whether any changed path is one the suite reads, and why.
export function touched(changed: ReadonlyArray<string>, derived: Derived): Answer {
  if (derived.unresolved.length > 0) {
    return {
      touched: true,
      why: `cannot judge — ${derived.unresolved.length} file(s) read what this scan cannot see, so the suite runs:\n` +
        derived.unresolved.map((u) => `  ${u}`).join('\n'),
    };
  }
  const under = (path: string, r: string) => path === r || path.startsWith(r.endsWith('/') ? r : `${r}/`);
  for (const path of changed) {
    // The longest read that holds the path names the file that reads it most exactly.
    const read = derived.reads.filter((r) => under(path, r)).sort((a, b) => b.length - a.length)[0];
    if (read !== undefined) {
      const by = derived.readers[read] ?? [];
      return { touched: true, why: `${path} is read by ${by.slice(0, 3).join(', ')}${by.length > 3 ? ` and ${by.length - 3} more` : ''}` };
    }
  }
  return {
    touched: false,
    why: `none of ${changed.length} changed path(s) is among the ${derived.reads.length} the web suite reads`,
  };
}

// web-suite-reads: none — the gate's reader of the suite, which the suite never loads
async function main(args: ReadonlyArray<string>): Promise<number> {
  // The repository is three levels above this script, wherever it runs from.
  const repo = resolve(dirname(Bun.main), '..', '..', '..');
  const derived = webSuiteReads(repo);
  if (args[0] === '--touched') {
    const changed = (await Bun.stdin.text()).split('\n').map((l) => l.trim()).filter(Boolean);
    const answer = touched(changed, derived);
    console.error(`web-suite-reads: ${answer.touched ? 'yes' : 'no'} — ${answer.why}`);
    console.log(answer.touched ? 'yes' : 'no');
    return 0;
  }
  for (const u of derived.unresolved) console.error(`web-suite-reads: UNRESOLVED ${u}`);
  for (const r of derived.reads) console.log(r);
  return derived.unresolved.length > 0 ? 1 : 0;
}

if (import.meta.main) process.exit(await main(process.argv.slice(2)));
