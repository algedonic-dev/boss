// c7720abe: a train passes its cluster gate AND forge CI. Comparing the
// local gate to forge alone rejected removal of a duplicate typecheck.
import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { webCoverage } from './web-ci-coverage';

const read = (path: string) => readFileSync(new URL(path, import.meta.url), 'utf8');
const current = {
  workflow: read('../../../../.forgejo/workflows/ci.yml'),
  gate: read('../../../../infra/gate.sh'),
  typecheck: read('../../../../infra/lint/svelte-check.sh'),
  scripts: (JSON.parse(read('../../package.json')) as { scripts: Record<string, string> }).scripts,
};
// Exact Typecheck-step removal from car62e4423/train644e8417.
const residual = { ...current, workflow: current.workflow.replace(
  /      - name: Typecheck\n        working-directory: apps\/web\n        run: bun run typecheck\n\n/, '',
) };
const removeStep = (workflow: string, name: string) => workflow.replace(
  new RegExp(`      - name: ${name}\\n[\\s\\S]*?(?=      - name:|\\n  # THE RUST|$)`), '',
);
type WorkflowFixture = {
  defaults?: unknown;
  jobs: { web: { defaults?: unknown; steps: { name?: string; run?: string; shell?: unknown }[] } };
};
const fixture = () => Bun.YAML.parse(residual.workflow) as WorkflowFixture;

describe('web coverage across both required train lanes', () => {
  test('current and assembled residual forge shapes cover the local gate', () => {
    expect(webCoverage(current)).toEqual([]);
    expect(webCoverage(residual)).toEqual([]);
  });
  test('typecheck cannot disappear from both lanes', () => {
    const gate = residual.gate.replace(/^check "svelte-check".*$/m, '');
    expect(webCoverage({ ...residual, gate }).join('\n')).toContain('typecheck');
  });
  test('AUTO-conditional typecheck does not supply the unconditional check', () => {
    const gate = residual.gate.replace(/^check "svelte-check".*$/m,
      (line) => `if [ "$AUTO" -eq 0 ]; then\n${line}\nfi`);
    expect(webCoverage({ ...residual, gate }).join('\n')).toContain('typecheck');
  });
  test('an uncalled function cannot hide the final train check', () => {
    const gate = residual.gate.replace('web_touched() {', 'uncalled() {\nweb_touched() {') + '\n}\n';
    expect(webCoverage({ ...residual, gate }).join('\n')).toContain('typecheck');
  });
  test('inert typecheck mentions supply nothing', () => {
    const gate = residual.gate.replace(/^check "svelte-check".*$/m, (line) => `# ${line}`);
    expect(webCoverage({ ...residual, gate }).join('\n')).toContain('typecheck');
    const typecheck = residual.typecheck.replace(/^bun run typecheck.*$/m, '# removed');
    expect(webCoverage({ ...residual, typecheck }).join('\n')).toContain('typecheck');
  });
  test('the typecheck carrier must run the check and propagate its failure', () => {
    const early = residual.typecheck.replace('out="$log_dir/svelte-check.out"', 'exit 0\nout="$log_dir/svelte-check.out"');
    const conditional = residual.typecheck.replace('out="$log_dir/svelte-check.out"', 'if false; then\nout="$log_dir/svelte-check.out"').replace('rc=$?', 'rc=$?\nfi');
    const ignored = residual.typecheck.replace('rc=$?', 'rc=0');
    const uncalled = `uncalled() {\n${residual.typecheck}\n}\n`;
    const changedArgv = residual.typecheck.replace('bun run typecheck --output machine', 'bun "run typecheck" "--output machine"');
    for (const typecheck of [early, conditional, ignored, uncalled, changedArgv]) {
      expect(webCoverage({ ...residual, typecheck }).join('\n')).toContain('typecheck');
    }
  });
  test('the gate dispatcher must execute typecheck and record its failure', () => {
    const skipped = residual.gate.replace('check() {', 'check() {\n    if [ "$1" = "svelte-check" ]; then return 0; fi');
    const inertRunner = residual.gate.replace('check_run() {', 'check_run() {\n    return 0');
    const ignoredFailure = residual.gate.replace('CHECK_STATUS=$?', 'CHECK_STATUS=0');
    for (const gate of [skipped, inertRunner, ignoredFailure]) {
      expect(gate).not.toEqual(residual.gate);
      expect(webCoverage({ ...residual, gate }).join('\n')).toContain('typecheck');
    }
  });
  test('unit steps retain their working-directory identities', () => {
    for (const [name, identity] of [['Unit tests', 'apps/web:test:unit'], ['web-kit unit tests', 'libs/web-kit:test:unit']]) {
      expect(webCoverage({ ...residual, workflow: removeStep(residual.workflow, name!) }).join('\n')).toContain(identity!);
    }
  });
  test('build and mocked browser checks stay on the residual lane', () => {
    for (const name of ['Build', 'Frontend smoke \\(mocked backend\\)']) {
      expect(webCoverage({ ...residual, workflow: removeStep(residual.workflow, name) }).length).toBeGreaterThan(0);
    }
  });
  test('conditional, wrong-directory, empty and malformed workflow reads refuse', () => {
    const conditional = residual.workflow.replace('- name: Unit tests', '- name: Unit tests\n        if: false');
    const elsewhere = residual.workflow.replace(/(- name: Unit tests\n        working-directory:) apps\/web/, '$1 libs/web-kit');
    for (const workflow of [conditional, elsewhere, '', 'jobs: [']) {
      expect(webCoverage({ ...residual, workflow }).length).toBeGreaterThan(0);
    }
  });
  test('shell-conditional blocks and tolerated failures do not pay the contract', () => {
    const shellConditional = residual.workflow.replace(
      /(- name: Unit tests\n        working-directory: apps\/web\n        run:) bun run test:unit/,
      '$1 |\n          if false; then\n            bun run test:unit\n          fi',
    );
    const toleratedStep = residual.workflow.replace('- name: Unit tests', '- name: Unit tests\n        continue-on-error: true');
    const toleratedJob = residual.workflow.replace('  web:\n', '  web:\n    continue-on-error: true\n');
    for (const workflow of [shellConditional, toleratedStep, toleratedJob]) {
      expect(webCoverage({ ...residual, workflow }).length).toBeGreaterThan(0);
    }
  });
  test('unqualified step and inherited Actions shells supply no coverage', () => {
    for (const scope of ['step', 'job', 'workflow']) {
      for (const shell of ['/usr/bin/true {0}', 'bash -c true {0}', 'bash {0}', 'node {0}', '', null]) {
        const doc = fixture();
        if (scope === 'step') doc.jobs.web.steps.find((step) => step.name === 'Unit tests')!.shell = shell;
        else if (scope === 'job') doc.jobs.web.defaults = { run: { shell } };
        else doc.defaults = { run: { shell } };
        expect(webCoverage({ ...residual, workflow: Bun.YAML.stringify(doc) }).length).toBeGreaterThan(0);
      }
    }
  });
  test('effective shell follows qualified step, job and workflow precedence', () => {
    for (const shell of ['bash', 'sh', 'bash --noprofile --norc -e -o pipefail {0}', 'sh -e {0}']) {
      const doc = fixture();
      doc.defaults = { run: { shell } };
      expect(webCoverage({ ...residual, workflow: Bun.YAML.stringify(doc) })).toEqual([]);
      doc.defaults = { run: { shell: '/usr/bin/true {0}' } };
      doc.jobs.web.defaults = { run: { shell } };
      expect(webCoverage({ ...residual, workflow: Bun.YAML.stringify(doc) })).toEqual([]);
      doc.jobs.web.defaults = { run: { shell: '/usr/bin/true {0}' } };
      for (const step of doc.jobs.web.steps) if (step.run) step.shell = shell;
      expect(webCoverage({ ...residual, workflow: Bun.YAML.stringify(doc) })).toEqual([]);
    }
  });
  test('malformed shell defaults cannot silently become implicit defaults', () => {
    for (const value of [null, [], 'bash', { run: null }, { run: false }, { run: { shell: null } }]) {
      for (const scope of ['workflow', 'job']) {
        const doc = fixture();
        if (scope === 'workflow') doc.defaults = value;
        else doc.jobs.web.defaults = value;
        expect(webCoverage({ ...residual, workflow: Bun.YAML.stringify(doc) }).length).toBeGreaterThan(0);
      }
    }
  });
  test('both coverage directions and actual script existence remain checked', () => {
    const scripts = { ...residual.scripts, gate: `${residual.scripts.gate} && bun run missing` };
    expect(webCoverage({ ...residual, scripts }).join('\n')).toContain('missing');
    expect(webCoverage({ ...residual, scripts: { ...residual.scripts, gate: '' } }).length).toBeGreaterThan(0);
    const workflow = residual.workflow.replace('run: bun run build', 'run: bun run build\n      - name: New check\n        working-directory: apps/web\n        run: bun run newly-added');
    expect(webCoverage({ ...residual, workflow }).join('\n')).toContain('newly-added');
    const conditional = { ...residual.scripts, gate: `if false; then ${residual.scripts.gate}; fi` };
    expect(webCoverage({ ...residual, scripts: conditional }).length).toBeGreaterThan(0);
  });
});
