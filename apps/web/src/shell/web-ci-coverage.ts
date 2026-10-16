// Test-side reader for the actual two-lane frontend contract (c7720abe).
// Directory is part of a command's identity: web-kit:test:unit cannot pay
// apps/web:test:unit. Unknown/conditional shapes contribute no coverage.
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

// This deliberately recognizes the gate's current final, unconditional
// phase rather than pretending to parse arbitrary shell. A restructure
// refuses until this reader and its adverse controls are reviewed with it.
const unconditionalTypecheck = (gate: string, script: string): boolean => {
  const anchor = '\nfi\n\nrun_preflight\n';
  const offset = gate.lastIndexOf(anchor);
  if (offset < 0) return false;
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
  return lines[0] === 'run_preflight'
    && /^check "svelte-check"\s+infra\/lint\/svelte-check\.sh$/.test(lines[1] ?? '')
    && JSON.stringify(lines.slice(2)) === JSON.stringify(verdict)
    && dispatcherRunsTypecheck(gate)
    && carrierRunsTypecheck(script);
};

export const webCoverage = ({ workflow, gate, typecheck, scripts }: Inputs): readonly string[] => {
  let document: unknown;
  try { document = Bun.YAML.parse(workflow); } catch { return ['forge workflow is not readable YAML']; }
  const root = object(document);
  const web = object(object(root?.jobs)?.web);
  const steps = web?.steps;
  if (!web || 'if' in web || (web['continue-on-error'] !== undefined && web['continue-on-error'] !== false)
    || !Array.isArray(steps)) return ['forge web job is absent, conditional, failure-tolerant or unreadable'];
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
  // AUTO may skip the whole web phase for non-app inputs read by the specs
  // (a244c6e7). Until that changes, these residual forge checks must stay.
  for (const identity of ['apps/web:test:unit', 'apps/web:build', 'apps/web:test:mocked', 'libs/web-kit:test:unit']) {
    if (!forge.has(identity)) errors.push(`required residual forge check missing: ${identity}`);
  }
  const combined = new Set([...forge].filter((identity) => identity.startsWith('apps/web:')).map((identity) => identity.slice('apps/web:'.length)));
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
