// The pin on web-suite-reads.ts (backlog a244c6e7): the --auto gate
// runs the web app's build and mocked suite exactly when a car changes
// something they read, and what they read is DERIVED from their own
// files — so this pins the derivation, on the real tree and on small
// trees made to break each of its rules.
import { afterAll, describe, expect, test } from 'bun:test';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';

import { touched, webSuiteReads } from './web-suite-reads';

// The repository root — this file is apps/web/scripts/.
const REPO = new URL('../../../', import.meta.url).pathname;

describe('the real tree', () => {
  const derived = webSuiteReads(REPO);

  test('every read is one the derivation can see — nothing it must guess at', () => {
    // A file that builds a path from its own location or the cwd hides
    // what it reads from a scan; it must say what it reads in its own
    // text (`web-suite-reads:`), or the gate cannot judge a car by it.
    expect(derived.unresolved).toEqual([]);
  });

  test('the suite owns apps/web and the workspace packages it depends on', () => {
    expect(derived.reads).toContain('apps/web/');
    expect(derived.reads).toContain('libs/web-kit/');
    expect(derived.reads).not.toContain('apps/simulator/');
  });

  test('the install reads the workspace manifest, its lockfile and its patches', () => {
    expect(derived.reads).toContain('package.json');
    expect(derived.reads).toContain('bun.lock');
    expect(derived.reads.some((r) => r.startsWith('patches/'))).toBe(true);
  });

  test('the reads the packet measured outside those trees', () => {
    // The specs load the step-plugin bundles (17 reads, some by a
    // template over the directory), the estate's dev-door declaration,
    // and the bundle imports the architecture diagrams' source.
    expect(touched(['infra/step-plugins/sign-off.js'], derived).touched).toBe(true);
    expect(touched(['infra/step-plugins/a-plugin-no-spec-names-yet.js'], derived).touched).toBe(true);
    expect(touched(['infra/estate/dev-door.json'], derived).touched).toBe(true);
    expect(touched(['docs/architecture/02-service-map.mmd'], derived).touched).toBe(true);
  });

  test('a unit test file reads nothing on the suite’s behalf — the units run on every tree', () => {
    // estate.test.ts reads estate_alarm.rs; that is the unit suite's
    // read, judged on every gate, not a reason to run the browser.
    expect(touched(['crates/orchestrators/boss-dispatcher-handlers/src/handlers/estate_alarm.rs'], derived).touched)
      .toBe(false);
  });

  test('says which file reads the path that made it run', () => {
    const answer = touched(['crates/core/boss-jobs/src/lib.rs', 'infra/step-plugins/sign-off.js'], derived);
    expect(answer.touched).toBe(true);
    expect(answer.why).toContain('infra/step-plugins/sign-off.js');
    expect(answer.why).toMatch(/apps\/web\/(tests|src)\/\S+/);
  });

  test('a Rust-only car and an empty diff do not run it', () => {
    expect(touched(['crates/core/boss-jobs/src/lib.rs', 'infra/gate.sh'], derived).touched).toBe(false);
    expect(touched([], derived).touched).toBe(false);
  });
});

describe('the rules, each on a tree made to break it', () => {
  const made: string[] = [];
  afterAll(() => made.forEach((dir) => rmSync(dir, { recursive: true, force: true })));

  /// A workspace whose app depends on one kit package; `files` adds to it.
  function tree(files: Readonly<Record<string, string>>): string {
    const root = mkdtempSync(join(tmpdir(), 'web-suite-reads-'));
    made.push(root);
    const base: Record<string, string> = {
      'package.json': JSON.stringify({ workspaces: ['apps/*', 'libs/*'], patchedDependencies: { 'x@1': 'patches/x@1.patch' } }),
      'bun.lock': '{}',
      'apps/web/package.json': JSON.stringify({
        name: 'web',
        dependencies: { kit: 'workspace:*' },
        scripts: { 'test:unit': 'bun scripts/each-test-file-alone.ts src scripts' },
      }),
      'apps/other/package.json': JSON.stringify({ name: 'other' }),
      'libs/kit/package.json': JSON.stringify({ name: 'kit', scripts: { 'test:unit': 'bun ../../apps/web/scripts/each-test-file-alone.ts src' } }),
      'apps/web/src/main.ts': "import { k } from 'kit/x';\nexport const m = k;\n",
      ...files,
    };
    for (const [path, text] of Object.entries(base)) {
      mkdirSync(dirname(join(root, path)), { recursive: true });
      writeFileSync(join(root, path), text);
    }
    return root;
  }

  test('own trees come from the workspace dependencies, and the install inputs from the root manifest', () => {
    const { reads, unresolved } = webSuiteReads(tree({}));
    expect(unresolved).toEqual([]);
    expect(reads).toEqual(['apps/web/', 'bun.lock', 'libs/kit/', 'package.json', 'patches/x@1.patch']);
  });

  test('a relative literal that leaves the trees is a read; a template is read up to its first hole', () => {
    const { reads } = webSuiteReads(tree({
      'apps/web/tests/a.mocked.spec.ts':
        "const one = readFileSync(new URL('../../../infra/plugins/one.js', import.meta.url), 'utf8');\n" +
        'const any = (n: string) => readFileSync(new URL(`../../../infra/bundles/${n}.js`, import.meta.url));\n',
      'apps/web/src/kb.ts': "import d from '../../../docs/a.mmd' with { type: 'text' };\nexport default d;\n",
    }));
    expect(reads).toContain('infra/plugins/one.js');
    expect(reads).toContain('infra/bundles/');
    expect(reads).toContain('docs/a.mmd');
  });

  test('a comment that names a path reads nothing, and neither does a unit test', () => {
    const { reads, unresolved } = webSuiteReads(tree({
      'apps/web/src/note.ts': "// see '../../../infra/only-in-a-comment.sh' and import.meta.dir\nexport const n = 1;\n",
      'apps/web/src/note.test.ts': "readFileSync(new URL('../../../infra/unit-only.toml', import.meta.url));\n",
      'libs/kit/src/k.test.ts': "readFileSync(new URL('../../../infra/kit-unit-only.toml', import.meta.url));\n",
    }));
    expect(unresolved).toEqual([]);
    expect(reads.filter((r) => r.startsWith('infra/'))).toEqual([]);
  });

  test('a path built from the file’s place or the cwd is refused until the file says what it reads', () => {
    const spec = "const p = join(import.meta.dir, '..', '..', '..', 'infra', 'hidden.json');\n";
    const bare = webSuiteReads(tree({ 'apps/web/tests/b.mocked.spec.ts': spec }));
    expect(bare.unresolved).toHaveLength(1);
    expect(bare.unresolved[0]).toContain('apps/web/tests/b.mocked.spec.ts');
    expect(bare.unresolved[0]).toContain('import.meta.dir');
    // Unjudged, the gate runs the suite rather than skip it.
    expect(touched(['crates/x/src/lib.rs'], bare).touched).toBe(true);

    const declared = webSuiteReads(tree({
      'apps/web/tests/b.mocked.spec.ts': `// web-suite-reads: infra/hidden.json — the fixture the spec serves\n${spec}`,
    }));
    expect(declared.unresolved).toEqual([]);
    expect(declared.reads).toContain('infra/hidden.json');

    const none = webSuiteReads(tree({
      'apps/web/src/dev.ts': '// web-suite-reads: none — names this tree to the server, reads nothing\nexport const here = process.cwd();\n',
    }));
    expect(none.unresolved).toEqual([]);
  });

  test('each form that hides a read is refused', () => {
    for (const line of [
      'const a = import.meta.dirname;',
      'const a = import.meta.path;',
      'const a = __dirname;',
      'const a = process.cwd();',
      'const a = fileURLToPath(import.meta.url);',
      'const a = new URL(name, import.meta.url);',
      "const a = new URL('../../../', import.meta.url);",
    ]) {
      const { unresolved } = webSuiteReads(tree({ 'apps/web/src/c.ts': `${line}\nexport { a };\n` }));
      expect(unresolved, line).toHaveLength(1);
    }
  });

  test('a svelte component is read by its script, not its markup', () => {
    const { reads } = webSuiteReads(tree({
      'apps/web/src/D.svelte':
        "<script lang=\"ts\">\n  import src from '../../../docs/b.mmd' with { type: 'text' };\n</script>\n<p>{src}</p>\n",
    }));
    expect(reads).toContain('docs/b.mmd');
  });

  test('a changed path matches a read it equals or sits under, and nothing else', () => {
    const derived = { reads: ['apps/web/', 'infra/bundles/', 'docs/a.mmd'], unresolved: [], readers: {} };
    expect(touched(['infra/bundles/x.js'], derived).touched).toBe(true);
    expect(touched(['docs/a.mmd'], derived).touched).toBe(true);
    expect(touched(['apps/web/src/x.ts'], derived).touched).toBe(true);
    expect(touched(['docs/b.mmd', 'infra/other/x.js', 'apps/webby/x'], derived).touched).toBe(false);
  });
});
