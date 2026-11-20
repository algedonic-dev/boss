// c7720abe: a train passes its cluster gate AND forge CI. Comparing the
// local gate to forge alone rejected removal of a duplicate typecheck.
import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { STAMP_RUN, webCoverage } from './web-ci-coverage';

const read = (path: string) => readFileSync(new URL(path, import.meta.url), 'utf8');
const current = {
  workflow: read('../../../../.forgejo/workflows/ci.yml'),
  gate: read('../../../../infra/gate.sh'),
  typecheck: read('../../../../infra/lint/svelte-check.sh'),
  scripts: (JSON.parse(read('../../package.json')) as { scripts: Record<string, string> }).scripts,
};
// The two check lines through which the train gate pays for apps/web.
const WEB_SUITE = `check "web-suite (unit+build+mocked)" output_to "$web_out" bash -c 'cd apps/web && bun run test:unit && bun run build && bun run test:mocked'`;
const WEB_UNIT = `check "web unit" bash -c 'cd apps/web && bun run test:unit'`;
const RECIPE_GUARD = "        if: steps.recipe.outputs.changed != 'no'\n";
// THE RESIDUAL FORGE LANE, ALONE (81ee98b3): the kept web steps without
// their recipe guard, and a gate whose web phase pays for nothing, so
// every apps/web check must come from the forge job. The forge lane's
// adversarial controls run on this shape; the gate lane's on `current`.
const residual = {
  ...current,
  workflow: current.workflow.replaceAll(RECIPE_GUARD, ''),
  gate: current.gate.replace(WEB_SUITE, ':').replace(WEB_UNIT, ':'),
};
const removeStep = (workflow: string, name: string) => workflow.replace(
  new RegExp(`      - name: ${name}\\n[\\s\\S]*?(?=      - name:|\\n  # THE RUST|$)`), '',
);
const mutate = (text: string, from: string, to: string) => {
  expect(text).toContain(from);
  return text.replace(from, to);
};
type WorkflowFixture = {
  defaults?: unknown;
  jobs: {
    'delivery-plan': { steps: { id?: string; shell?: unknown; run?: string }[] };
    web: { defaults?: unknown; steps: { name?: string; run?: string; shell?: unknown }[] };
  };
};
const fixture = () => Bun.YAML.parse(residual.workflow) as WorkflowFixture;

describe('web coverage across both required train lanes', () => {
  test('software route coverage requires the exact upstream output and dependency', () => {
    for (const [before, after] of [
      ["if: needs.delivery-plan.outputs.route != 'data'", "if: false"],
      ["if: needs.delivery-plan.outputs.route != 'data'", "if: needs.delivery-plan.outputs.route == 'data'"],
      ['needs: [delivery-plan, locomotive]', 'needs: [locomotive]'],
      ['route: ${{ steps.plan.outputs.route }}', 'route: data'],
      ['id: plan', 'id: other'],
    ] as const) {
      expect(residual.workflow).toContain(before);
      const workflow = before.startsWith('if:')
        ? residual.workflow.split('  web:\n').map((part, i) => i === 1 ? part.replace(before, after) : part).join('  web:\n')
        : residual.workflow.replace(before, after);
      expect(webCoverage({ ...residual, workflow }).length).toBeGreaterThan(0);
    }
    for (const [before, after] of [
      ['  delivery-plan:\n', '  delivery-plan:\n    if: false\n'],
      ['  delivery-plan:\n', '  delivery-plan:\n    continue-on-error: true\n'],
      ['        id: plan\n', '        id: plan\n        if: false\n'],
      ['        id: plan\n', '        id: plan\n        continue-on-error: true\n'],
      ['        id: plan\n', '        id: plan\n        shell: /usr/bin/true {0}\n'],
      ['  delivery-plan:\n', '  delivery-plan:\n    defaults: {run: {shell: null}}\n'],
      ['          cat delivery-plan.json\n', '          cat delivery-plan.json\n          echo route=data >> "$GITHUB_OUTPUT"\n'],
    ] as const) {
      expect(webCoverage({ ...residual, workflow: residual.workflow.replace(before, after) }).length).toBeGreaterThan(0);
    }
  });
  // 399ee6c3, train 7474e3e0: delivery-plan's one other output producer
  // names the CI runner image. Admitted only as known text that writes
  // ci_stamp and nothing else; every other shape is a second writer.
  test('the runner-image stamp step is the one other producer, and only as known text', () => {
    const stampLine = '.hexdigest())\' >> "$GITHUB_OUTPUT"';
    expect(residual.workflow).toContain(stampLine);
    for (const [before, after] of [
      [stampLine, '.hexdigest() + "\\nroute=data")\' >> "$GITHUB_OUTPUT"'],
      [stampLine, `${stampLine}\n          echo route=data >> "$GITHUB_OUTPUT"`],
      ['        id: stamp\n', '        id: image\n'],
      ['        id: stamp\n', '        id: stamp\n        shell: bash -c \'echo route=data >> "$GITHUB_OUTPUT"\' {0}\n'],
    ] as const) {
      const workflow = residual.workflow.replace(before, after);
      expect(workflow).not.toEqual(residual.workflow);
      expect(webCoverage({ ...residual, workflow }).length).toBeGreaterThan(0);
    }
  });
  // 66aaea77: the stamp step's text lives in ci.yml AND in this reader as
  // STAMP_RUN, on purpose — the reader admits it as exact text, so a
  // change to the step is a change to the reader, reviewed together. The
  // pin below holds the two equal; the mutation shows a drift NAMES the
  // stamp text rather than reading as an absent web job, which is what
  // it said until this car.
  test('the stamp step ci.yml runs is the exact text the reader admits', () => {
    const stamp = fixture().jobs['delivery-plan'].steps.find((step) => step.id === 'stamp');
    expect(stamp?.run?.trim()).toBe(STAMP_RUN);
    const harmless = mutate(residual.workflow, '("Dockerfile", "required-tools.txt")',
      '("Dockerfile", "required-tools.txt", "build.sh")');
    const errors = webCoverage({ ...residual, workflow: harmless }).join('\n');
    expect(errors).toContain('STAMP_RUN');
    expect(errors).not.toContain('forge web job is absent');
  });
  test('current and assembled residual forge shapes cover the local gate', () => {
    expect(webCoverage(current)).toEqual([]);
    expect(webCoverage(residual)).toEqual([]);
    // Each lane alone: the residual shape is not the current one twice.
    expect(residual.workflow).not.toEqual(current.workflow);
    expect(residual.gate).not.toEqual(current.gate);
  });
  test('the forge web steps run only on a new recipe, so without the gate lane nothing pays', () => {
    const errors = webCoverage({ ...current, gate: residual.gate }).join('\n');
    for (const command of ['test:unit', 'build', 'test:mocked']) expect(errors).toContain(`required train lanes lost ${command}`);
  });
  test('the gate lane pays only through its exact web phase', () => {
    const open = 'if [ "$AUTO" -eq 0 ] || [ "$(web_touched)" = "yes" ]; then';
    for (const [gate, lost] of [
      // A check line gone from either branch.
      [mutate(current.gate, WEB_SUITE, ':'), ['test:unit', 'build', 'test:mocked']],
      [mutate(current.gate, WEB_UNIT, ':'), ['test:unit']],
      // A check line that changed, nested, or continues a command.
      [mutate(current.gate, WEB_SUITE, WEB_SUITE.replace('&& bun run build &&', '&& (bun run build || true) &&')), ['build']],
      [mutate(current.gate, WEB_SUITE, `if false; then\n    ${WEB_SUITE}\n    fi`), ['build', 'test:mocked']],
      [mutate(current.gate, WEB_SUITE, `false &&\n    ${WEB_SUITE}`), ['build', 'test:mocked']],
      [mutate(current.gate, "\nelse\n    # apps/web's unit files", "\nelif false; then\n    # apps/web's unit files"), ['test:unit']],
      // A narrower condition, or a derivation that answers no when it cannot answer.
      [mutate(current.gate, open, 'if [ "$AUTO" -eq 0 ]; then'), ['build', 'test:mocked']],
      [mutate(current.gate, 'so the web suite runs" >&2; echo yes ;;', 'so the web suite runs" >&2; echo no ;;'), ['build', 'test:mocked']],
      // output_to that does not run the suite, or drops its failure.
      [mutate(current.gate, 'return "${PIPESTATUS[0]}"', 'return 0'), ['build', 'test:mocked']],
      [mutate(current.gate, 'output_to() { # <file, or empty> <cmd...>: run it, its output ALSO written to the file\n',
        'output_to() { # <file, or empty> <cmd...>: run it, its output ALSO written to the file\n    return 0\n'), ['build', 'test:mocked']],
    ] as const) {
      expect(gate).not.toEqual(current.gate);
      const errors = webCoverage({ ...current, gate }).join('\n');
      for (const command of lost) expect(errors).toContain(`required train lanes lost ${command}`);
    }
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
  test('an uncalled function cannot hide the final train checks', () => {
    for (const lane of [residual, current]) {
      const gate = lane.gate.replace('web_touched() {', 'uncalled() {\nweb_touched() {') + '\n}\n';
      expect(webCoverage({ ...lane, gate }).join('\n')).toContain('typecheck');
    }
    const gate = current.gate.replace('web_touched() {', 'uncalled() {\nweb_touched() {') + '\n}\n';
    expect(webCoverage({ ...current, gate }).join('\n')).toContain('required train lanes lost build');
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
  test('a forge unit step keeps its working-directory identity', () => {
    // web-kit's step runs the same command from libs/web-kit; it cannot
    // pay for apps/web's unit files.
    expect(webCoverage({ ...residual, workflow: removeStep(residual.workflow, 'Unit tests') }).join('\n'))
      .toContain('required train lanes lost test:unit');
  });
  test('build and mocked browser checks stay on the forge lane when it is the only one', () => {
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
      expect(workflow).not.toEqual(residual.workflow);
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
      // The route producer is a separate job. Isolate web-shell precedence
      // from its interpreter so a true workflow shell cannot kill it too.
      doc.jobs['delivery-plan'].steps.find((step) => step.id === 'plan')!.shell = 'bash';
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
