// Test-side reader for the actual two-lane frontend contract (c7720abe).
// Directory is part of a command's identity: web-kit:test:unit cannot pay
// apps/web:test:unit. Unknown/conditional shapes contribute no coverage,
// except the one conditional the gate lane below qualifies (81ee98b3).
import { spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

type Inputs = Readonly<{
  workflow: string;
  gate: string;
  typecheck: string;
  scripts: Readonly<Record<string, string>>;
}>;
const object = (value: unknown): Record<string, unknown> | undefined =>
  typeof value === 'object' && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown> : undefined;
const commands = (value: string) => [...value.matchAll(/\bbun run ([a-z][a-z0-9:-]*)\b/g)].map((match) => match[1]!);

// Absence inherits; malformed declarations do not. Forgejo permits custom
// shell argv and workflow/job defaults: run text alone proves no execution
// when the effective interpreter is, for example, true {0} (review b8f3054a).
const shellDefault = (owner: Record<string, unknown>): string | undefined | null => {
  if (!('defaults' in owner)) return undefined;
  const defaults = object(owner.defaults);
  if (!defaults) return null;
  if (!('run' in defaults)) return undefined;
  const run = object(defaults.run);
  if (!run) return null;
  if (!('shell' in run)) return undefined;
  return typeof run.shell === 'string' ? run.shell : null;
};
const qualifiedShell = (shell: unknown): boolean => shell === undefined
  || (typeof shell === 'string' && [
    'bash', 'sh', 'bash --noprofile --norc -e -o pipefail {0}', 'sh -e {0}',
  ].includes(shell));

// This reader compares the software branch of the train with the local
// software gate. Only the one channel predicate is qualified here; the
// selector's real frozen-base execution and adverse inputs are owned by
// infra/forge/test_delivery_ci_workflow.py. Arbitrary job conditions still
// supply nothing. Bind the named dependency through its actual output to
// its non-conditional, failure-propagating producer (urgent271a2f6e).
// The one other output producer delivery-plan may carry (399ee6c3): it
// names the CI runner image by its recipe, so build-image (no checkout),
// locomotive and web read one name. It is admitted as exact text that can
// write ci_stamp and nothing else, with no shell of its own; any other
// shape is a second writer and refuses. Train 7474e3e0 (2026-10-20) went
// red when this step reached the reader unannounced. The line is held
// equal to locomotive.sh's stamp by boss-testing's
// the_ci_image_is_named_by_its_recipe, and to this text by the test here.
export const STAMP_RUN = 'set -eu\npython3 -I -c \'import hashlib; print("ci_stamp=" + hashlib.sha256(b"".join('
  + 'open("infra/forge/boss-ci/" + n, "rb").read() for n in ("Dockerfile", "required-tools.txt"))).hexdigest())\''
  + ' >> "$GITHUB_OUTPUT"';
const stampStep = (step: Record<string, unknown> | undefined): boolean => step?.id === 'stamp'
  && !('shell' in step) && typeof step.run === 'string' && step.run.trim() === STAMP_RUN;
// A refused stamp step NAMES itself (66aaea77). The text lives here and in
// ci.yml on purpose — admitting it exactly is what makes a change to the
// step a change to this reader, reviewed with it — so the two drifting is
// expected, and it read as 'forge web job is absent' until this said why.
const STAMP_DRIFT = "forge delivery-plan's `stamp` step is not the exact text this reader admits"
  + ' (STAMP_RUN in apps/web/src/shell/web-ci-coverage.ts), or it declares its own shell;'
  + ' change the step and STAMP_RUN together';
const stampDrift = (root: Record<string, unknown>): string | undefined => {
  const steps = object(object(root.jobs)?.['delivery-plan'])?.steps;
  const stamps = Array.isArray(steps) ? steps.map(object).filter((step) => step?.id === 'stamp') : [];
  return stamps.some((step) => !stampStep(step)) ? STAMP_DRIFT : undefined;
};

const softwareLane = (root: Record<string, unknown>, web: Record<string, unknown>): boolean => {
  if (!('if' in web)) return true;
  if (web.if !== "needs.delivery-plan.outputs.route != 'data'"
    || !Array.isArray(web.needs) || !web.needs.includes('delivery-plan')) return false;
  const plan = object(object(root.jobs)?.['delivery-plan']);
  if (!plan || 'if' in plan || (plan['continue-on-error'] !== undefined && plan['continue-on-error'] !== false)
    || object(plan.outputs)?.route !== '${{ steps.plan.outputs.route }}'
    || !Array.isArray(plan.steps)) return false;
  const producers = plan.steps.map(object).filter((step) => step?.id === 'plan');
  if (producers.length !== 1) return false;
  const producer = producers[0]!;
  const planShell = shellDefault(plan);
  const workflowShell = shellDefault(root);
  if (planShell === null || workflowShell === null
    || plan.steps.map(object).some((step) => step !== producer && !stampStep(step)
      && typeof step?.run === 'string' && step.run.includes('GITHUB_OUTPUT'))) return false;
  if ('if' in producer || (producer['continue-on-error'] !== undefined && producer['continue-on-error'] !== false)
    || typeof producer.run !== 'string'
    || !qualifiedShell('shell' in producer ? producer.shell : planShell ?? workflowShell)) return false;
  // The route must be emitted by the selected-plan step, not a literal
  // output on a similarly named job. The bootstrap is separately executed
  // against adversarial git trees; this recognizes its output wire only.
  const output = 'printf \'route=%s\\n\' "$(python3 -c \'import json; print(json.load(open("delivery-plan.json"))["route"])\')" >> "$GITHUB_OUTPUT"';
  return producer.run.trimEnd().endsWith(output)
    && producer.run.split('GITHUB_OUTPUT').length === 2;
};

// Exercise the actual wrapper with no package install or real typecheck.
// The private bun accepts only the two known commands and records the
// invocation. Both success AND a forced compiler failure must propagate.
const carrierRunsTypecheck = (script: string): boolean => {
  const directory = mkdtempSync(join(tmpdir(), 'boss-web-typecheck-'));
  try {
    for (const path of ['infra/lint', 'apps/web', 'bin']) mkdirSync(join(directory, path), { recursive: true });
    const wrapper = join(directory, 'infra/lint/svelte-check.sh');
    writeFileSync(wrapper, script);
    writeFileSync(join(directory, 'apps/web/svelte-check-warnings.txt'), '# synthetic zero-warning baseline\n');
    // This executable fixture never evaluates input or contacts a remote.
    writeFileSync(join(directory, 'bin/bun'), '#!/bin/sh\n' +
      'case "$#:${1-}:${2-}:${3-}:${4-}" in\n' +
      '  "2:install:--frozen-lockfile::") exit 0;;\n' +
      '  "4:run:typecheck:--output:machine")\n' +
      '    printf "%s\\n" typecheck >> "$CONTROL_LOG"\n' +
      '    printf "%s\\n" "0 COMPLETED 1 FILES 0 ERRORS 0 WARNINGS 0 FILES_WITH_PROBLEMS"\n' +
      '    exit "$CONTROL_EXIT";;\n' +
      '  *) exit 97;;\n' +
      'esac\n', { mode: 0o700 });
    for (const exit of [0, 1]) {
      const log = join(directory, `control-${exit}.log`);
      writeFileSync(log, '');
      // GNU timeout gives this controlled invocation its own process group
      // and kills remaining children after the grace bound. The outer bound
      // also refuses if the timeout executable itself cannot finish.
      const result = spawnSync('/usr/bin/timeout', ['--kill-after=1s', '20s', '/bin/bash', wrapper], {
        cwd: directory,
        env: { PATH: `${directory}/bin:/usr/bin:/bin`, HOME: directory, TMPDIR: directory,
          CONTROL_LOG: log, CONTROL_EXIT: String(exit), LC_ALL: 'C' },
        timeout: 22_000,
        maxBuffer: 262_144,
      });
      // A timeout kills the wrapper; also end any child left in its own
      // process group before removing this leg's private files.
      if (result.pid > 0) {
        try { process.kill(-result.pid, 'SIGKILL'); } catch (error) {
          if ((error as NodeJS.ErrnoException).code !== 'ESRCH') return false;
        }
      }
      const invocation = readFileSync(log);
      console.log('typecheck-carrier-control', JSON.stringify({
        expected_exit: exit, actual_exit: result.status, signal: result.signal,
        error: result.error?.message ?? null,
        stdout_b64: result.stdout?.toString('base64') ?? '',
        stderr_b64: result.stderr?.toString('base64') ?? '',
        invocation_b64: invocation.toString('base64'),
      }));
      if (result.error || result.status !== exit || invocation.toString('utf8') !== 'typecheck\n') return false;
    }
    return true;
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
};

// Qualify the actual dispatcher too: a live wrapper cannot pay for a
// check() that returns before calling it (review c089134b). Extract only
// the two current, uniquely named helpers; unknown layouts refuse.
const dispatcherRunsTypecheck = (gate: string): boolean => {
  const helpers: string[] = [];
  for (const name of ['check_run', 'check']) {
    const matches = [...gate.matchAll(new RegExp(`^${name}\\(\\) \\{[^\\n]*\\n[\\s\\S]*?^\\}`, 'gm'))];
    if (matches.length !== 1) return false;
    helpers.push(matches[0]![0]);
  }
  const directory = mkdtempSync(join(tmpdir(), 'boss-web-dispatch-'));
  try {
    const command = join(directory, 'typecheck');
    writeFileSync(command, '#!/bin/sh\nprintf "%s\\n" typecheck >> "$CONTROL_LOG"\nexit "$CONTROL_EXIT"\n', { mode: 0o700 });
    const wrapper = join(directory, 'dispatch.sh');
    writeFileSync(wrapper, [
      'set -u',
      'GATE_ANNOTATE=0; CHECK_STATUS=0; CHECK_TOOK=""; CHECK_REFUSES_ON=""',
      'FAILED=(); RAN=()',
      'runtime_evidence_sample() { :; }',
      'require_headroom() { :; }',
      'gh_annotate_fail() { :; }',
      ...helpers,
      'check "svelte-check" "$CONTROL_COMMAND"',
      '[ "$(cat "$CONTROL_LOG")" = typecheck ] || exit 91',
      '[ "$CHECK_STATUS" = "$CONTROL_EXIT" ] || exit 92',
      '[ "${#RAN[@]}" = 1 ] || exit 93',
      'if [ "$CONTROL_EXIT" = 0 ]; then',
      '  [ "${#FAILED[@]}" = 0 ] || exit 94',
      '  [[ "${RAN[0]}" = svelte-check:pass:* ]] || exit 95',
      'else',
      '  [ "${#FAILED[@]}" = 1 ] && [ "${FAILED[0]}" = svelte-check ] || exit 96',
      '  [[ "${RAN[0]}" = svelte-check:fail:* ]] || exit 97',
      'fi',
    ].join('\n') + '\n');
    for (const exit of [0, 17]) {
      const log = join(directory, `control-${exit}.log`);
      writeFileSync(log, '');
      const result = spawnSync('/usr/bin/timeout', ['--kill-after=1s', '20s', '/bin/bash', wrapper], {
        cwd: directory,
        env: { PATH: '/usr/bin:/bin', HOME: directory, TMPDIR: directory, LC_ALL: 'C',
          CONTROL_COMMAND: command, CONTROL_LOG: log, CONTROL_EXIT: String(exit) },
        timeout: 22_000,
        maxBuffer: 262_144,
      });
      if (result.pid > 0) {
        try { process.kill(-result.pid, 'SIGKILL'); } catch (error) {
          if ((error as NodeJS.ErrnoException).code !== 'ESRCH') return false;
        }
      }
      console.log('typecheck-dispatch-control', JSON.stringify({
        compiler_exit: exit, actual_exit: result.status, signal: result.signal,
        error: result.error?.message ?? null,
        stdout_b64: result.stdout?.toString('base64') ?? '',
        stderr_b64: result.stderr?.toString('base64') ?? '',
        invocation_b64: readFileSync(log).toString('base64'),
      }));
      if (result.error || result.status !== 0) return false;
    }
    return true;
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
};

// Qualify output_to, which carries the web-suite check's command: it must
// run the command, keep its output, and hand back the command's status on
// success AND failure — a tee'd pipeline returns tee's unless it says not
// to. Extract the one helper; unknown layouts refuse, as above.
const outputToPropagates = (gate: string): boolean => {
  const matches = [...gate.matchAll(/^output_to\(\) \{[^\n]*\n[\s\S]*?^\}/gm)];
  if (matches.length !== 1) return false;
  const directory = mkdtempSync(join(tmpdir(), 'boss-web-output-to-'));
  try {
    const command = join(directory, 'suite');
    writeFileSync(command, '#!/bin/sh\nprintf "%s\\n" suite >> "$CONTROL_LOG"\necho suite-output\nexit "$CONTROL_EXIT"\n', { mode: 0o700 });
    const wrapper = join(directory, 'output-to.sh');
    writeFileSync(wrapper, [
      'set -u',
      matches[0]![0],
      'output_to "$CONTROL_OUT" "$CONTROL_COMMAND"',
      'rc=$?',
      '[ "$rc" = "$CONTROL_EXIT" ] || exit 91',
      '[ "$(cat "$CONTROL_LOG")" = suite ] || exit 92',
      'grep -qx suite-output "$CONTROL_OUT" || exit 93',
    ].join('\n') + '\n');
    for (const exit of [0, 17]) {
      const log = join(directory, `control-${exit}.log`);
      const out = join(directory, `control-${exit}.out`);
      writeFileSync(log, '');
      writeFileSync(out, '');
      const result = spawnSync('/usr/bin/timeout', ['--kill-after=1s', '20s', '/bin/bash', wrapper], {
        cwd: directory,
        env: { PATH: '/usr/bin:/bin', HOME: directory, TMPDIR: directory, LC_ALL: 'C',
          CONTROL_COMMAND: command, CONTROL_LOG: log, CONTROL_OUT: out, CONTROL_EXIT: String(exit) },
        timeout: 22_000,
        maxBuffer: 262_144,
      });
      if (result.pid > 0) {
        try { process.kill(-result.pid, 'SIGKILL'); } catch (error) {
          if ((error as NodeJS.ErrnoException).code !== 'ESRCH') return false;
        }
      }
      console.log('web-suite-carrier-control', JSON.stringify({
        command_exit: exit, actual_exit: result.status, signal: result.signal,
        error: result.error?.message ?? null,
        stdout_b64: result.stdout?.toString('base64') ?? '',
        stderr_b64: result.stderr?.toString('base64') ?? '',
        invocation_b64: readFileSync(log).toString('base64'),
      }));
      if (result.error || result.status !== 0) return false;
    }
    return true;
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
};

// The gate's web phase closes right before this anchor, and the final,
// unconditional phase follows it.
const FINAL_PHASE_ANCHOR = '\nfi\n\nrun_preflight\n';

// This deliberately recognizes the gate's current final, unconditional
// phase rather than pretending to parse arbitrary shell. A restructure
// refuses until this reader and its adverse controls are reviewed with it.
// Consuming the WHOLE suffix, its verdict included, is what keeps both
// this phase and the web phase before it out of an uncalled function or
// an enclosing if: either would need a line after the verdict.
const finalPhase = (gate: string): readonly string[] | undefined => {
  const offset = gate.lastIndexOf(FINAL_PHASE_ANCHOR);
  if (offset < 0) return undefined;
  const tail = gate.slice(offset + '\nfi\n\n'.length);
  const lines = tail.split('\n').map((line) => line.trim()).filter((line) => line && !line.startsWith('#'));
  // Consume the complete known suffix, including its final verdict. Extra
  // fi/brace/commands cannot hide this phase inside an uncalled function.
  const verdict = [
    'if [ "${#FAILED[@]}" -gt 0 ]; then',
    'write_receipt "failed"',
    'echo "" >&2',
    'echo "gate: ${#FAILED[@]} check(s) failed: ${FAILED[*]}" >&2',
    'echo "gate: receipt written to ${GATE_RECEIPT}" >&2',
    'exit 1',
    'fi',
    'write_receipt "green"',
    'echo "gate: all checks green"',
    'echo "gate: receipt written to ${GATE_RECEIPT}"',
  ];
  return lines[0] === 'run_preflight' && JSON.stringify(lines.slice(2)) === JSON.stringify(verdict)
    ? lines : undefined;
};

const unconditionalTypecheck = (gate: string, script: string): boolean =>
  /^check "svelte-check"\s+infra\/lint\/svelte-check\.sh$/.test(finalPhase(gate)?.[1] ?? '')
    && dispatcherRunsTypecheck(gate)
    && carrierRunsTypecheck(script);

// THE GATE LANE (backlog 81ee98b3, 2026-10-20). The forge's web steps run
// only on a train that changes the runner image's recipe, so on every
// other train apps/web's unit, build and mocked checks are paid by the
// train's own cluster gate, or by nothing. Recognized, not parsed, like
// the final phase: the web phase that closes at FINAL_PHASE_ANCHOR (whose
// whole suffix must read), opened by exactly WEB_PHASE, its condition the
// exact derived web_touched, and an if/else whose two check lines sit at
// the top of their own branch — not nested, not the tail of a continued
// command. Anything else credits nothing.
//
// IT CREDITS A CONDITIONAL LANE, which this reader otherwise refuses:
// in --auto the build and the mocked suite run only when web_touched says
// yes. That is design cdf626ac's "prove each tree once" — web_touched is
// derived from what the build and specs read (web-suite-reads.ts, held by
// its own test and by an_auto_gate_runs_the_web_suite_on_what_it_reads.rs)
// and answers yes whenever it cannot answer, so a train that skips them
// changed nothing they read since the tree that last ran them. test:unit
// is credited only because BOTH branches run it. The condition is held
// exactly, so a narrower one, or a fallback that answers no, credits
// nothing.
const WEB_PHASE = 'if [ "$AUTO" -eq 0 ] || [ "$(web_touched)" = "yes" ]; then';
const WEB_TOUCHED = [
  'web_touched() {',
  'local answer',
  'answer=$(changed_paths | (cd apps/web && bun scripts/web-suite-reads.ts --touched)) || answer=""',
  'case "$answer" in',
  'yes|no) echo "$answer" ;;',
  '*) echo "gate: web-suite-reads gave no answer, so the web suite runs" >&2; echo yes ;;',
  'esac',
  '}',
];
const WEB_SUITE = `check "web-suite (unit+build+mocked)" output_to "$web_out" bash -c 'cd apps/web && bun run test:unit && bun run build && bun run test:mocked'`;
const WEB_UNIT = `check "web unit" bash -c 'cd apps/web && bun run test:unit'`;

const gateLane = (gate: string): readonly string[] => {
  const lines = gate.split('\n');
  const at = (text: string) => lines.flatMap((line, index) => line === text ? [index] : []);
  const defined = at(WEB_TOUCHED[0]!);
  const opened = at(WEB_PHASE);
  const anchor = gate.lastIndexOf(FINAL_PHASE_ANCHOR);
  if (defined.length !== 1 || opened.length !== 1 || anchor < 0 || !finalPhase(gate)) return [];
  const definition = lines.slice(defined[0]!, defined[0]! + WEB_TOUCHED.length).map((line) => line.trim());
  if (JSON.stringify(definition) !== JSON.stringify(WEB_TOUCHED)) return [];
  const closed = gate.slice(0, anchor + 1).split('\n').length - 1; // the phase's `fi`
  if (opened[0]! >= closed) return [];
  const body = lines.slice(opened[0]! + 1, closed).map((line) => line.trim())
    .filter((line) => line && !line.startsWith('#'));
  // Each branch's top-level commands; `if`/`fi` depth inside the phase.
  const branches: string[][] = [[]];
  let depth = 0;
  let continued = false;
  for (const line of body) {
    if (depth === 0 && line === 'else') {
      if (branches.length === 2) return [];
      branches.push([]);
      continued = false;
      continue;
    }
    if (depth === 0 && /^(elif|else|fi|for|while|until|case|select)\b/.test(line)) return [];
    const opens = /^if\b/.test(line) && !line.startsWith('if (');
    const closes = line.split(/[\s;]+/).some((token) => token === 'fi' || token === 'fi)');
    if (depth === 0 && !continued) branches.at(-1)!.push(line);
    depth += Number(opens) - Number(closes);
    if (depth < 0) return [];
    continued = /(&&|\|\||\||\\)$/.test(line);
  }
  if (depth !== 0 || branches.length !== 2) return [];
  const [then, otherwise] = branches as [string[], string[]];
  if (!then.includes(WEB_SUITE) || !outputToPropagates(gate) || !dispatcherRunsTypecheck(gate)) return [];
  return otherwise.includes(WEB_UNIT) ? ['test:unit', 'build', 'test:mocked'] : ['build', 'test:mocked'];
};

export const webCoverage = ({ workflow, gate, typecheck, scripts }: Inputs): readonly string[] => {
  let document: unknown;
  try { document = Bun.YAML.parse(workflow); } catch { return ['forge workflow is not readable YAML']; }
  const root = object(document);
  const web = object(object(root?.jobs)?.web);
  const steps = web?.steps;
  if (!web || !softwareLane(root!, web) || (web['continue-on-error'] !== undefined && web['continue-on-error'] !== false)
    || !Array.isArray(steps)) {
    return [(root && stampDrift(root)) ?? 'forge web job is absent, conditional, failure-tolerant or unreadable'];
  }
  const workflowShell = shellDefault(root!);
  const jobShell = shellDefault(web);
  if (workflowShell === null || jobShell === null) return ['forge shell defaults are unreadable'];
  const forge = new Set<string>();
  for (const value of steps) {
    const step = object(value);
    if (!step || 'if' in step || (step['continue-on-error'] !== undefined && step['continue-on-error'] !== false)
      || typeof step.run !== 'string' || typeof step['working-directory'] !== 'string') continue;
    const shell = 'shell' in step ? step.shell : jobShell ?? workflowShell;
    if (!qualifiedShell(shell)) continue;
    // Judge the WHOLE block: a bun line inside `if false` contributes
    // nothing. Unknown shell shapes refuse; this is not a shell parser.
    if (!/^bun run [a-z][a-z0-9:-]*(?:\s*&&\s*bun run [a-z][a-z0-9:-]*)*$/.test(step.run.trim())) continue;
    for (const command of commands(step.run)) forge.add(`${step['working-directory']}:${command}`);
  }
  const errors: string[] = [];
  // The forge used to be REQUIRED to carry unit, build and mocked (and
  // web-kit's unit) because --auto skipped the whole web phase on trees
  // the specs still read. Since a244c6e7 the gate lane pays for them, and
  // the forge runs them only on a new runner recipe (81ee98b3); web-kit's
  // unit suite runs on every tree, held by the gate's own web-phase pin.
  const combined = new Set([
    ...[...forge].filter((identity) => identity.startsWith('apps/web:')).map((identity) => identity.slice('apps/web:'.length)),
    ...gateLane(gate),
  ]);
  if (unconditionalTypecheck(gate, typecheck)) combined.add('typecheck');
  const chain = (scripts.gate ?? '').split(/\s*&&\s*/);
  if (chain.some((command) => !/^bun run [a-z][a-z0-9:-]*$/.test(command))) {
    errors.push('local gate is not an unconditional bun-script chain');
  }
  const local = new Set(chain.flatMap((command) =>
    /^bun run [a-z][a-z0-9:-]*$/.test(command) ? commands(command) : []));
  for (const command of ['typecheck', 'test:unit', 'build', 'test:mocked']) {
    if (!local.has(command)) errors.push(`local gate lost ${command}`);
    if (!combined.has(command)) errors.push(`required train lanes lost ${command}`);
  }
  for (const command of new Set([...combined, ...local])) {
    if (!(command in scripts)) errors.push(`undefined apps/web script: ${command}`);
    if (!local.has(command)) errors.push(`train runs ${command} but local gate does not`);
    if (!combined.has(command)) errors.push(`local gate runs ${command} but neither train lane does`);
  }
  return errors;
};
