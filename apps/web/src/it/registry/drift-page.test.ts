import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

// The Drift tab renders the newest `maintenance-protocol-drift` packet
// (4ae9969e, car 2 of 8f4e9cc0). This pins its three honest states to
// the template the way yard-status-page.test.ts pins the yard: the
// states are the ones the packet named, and each must be told apart
// from the others in the markup — a read that failed, a cadence that
// has not filed yet, and a packet that exists with no measurement are
// three different facts, and an empty table would collapse all three
// into "0 adrift", which is the confident wrong answer (CLAUDE.md
// §Doors).
describe('the registry drift page tells its three empty states apart', () => {
  const src = readFileSync(join(import.meta.dir, 'ProtocolDriftPage.svelte'), 'utf8');
  const markup = src.slice(src.indexOf('</script>'));

  it('reads the kind once, through the parser, at the fetch call site', () => {
    expect(src).toContain("from './drift'");
    expect(src).toContain('loadDriftPackets(');
    expect(src).toContain('newestMeasured(');
  });

  it('a failed read carries the shared failure marker and never an empty table', () => {
    const failed = markup.indexOf("page.kind === 'failed'");
    expect(failed).toBeGreaterThan(-1);
    const arm = markup.slice(failed, markup.indexOf('{:else', failed));
    expect(arm).toContain('load-failed');
    expect(arm).toContain('{page.error}');
    expect(arm).not.toContain('<table');
  });

  it('no packet yet states the cadence that has not filed, with no date', () => {
    // 3a8ab64c: the arm used to say the first run "was expected
    // 2026-09-15 05:20Z … train 376". The record's oldest packet opened
    // 2026-09-17, and the arm renders only when the kind is empty, so a
    // date there can only ever be stale. It states the cadence instead.
    const none = markup.indexOf('ready.total === 0');
    expect(none).toBeGreaterThan(-1);
    const arm = markup.slice(none, markup.indexOf('{:else', none));
    expect(arm).toContain('the daily 05:20Z measurement on boss-gcp has not filed a packet');
    expect(arm).not.toMatch(/20\d\d-\d\d-\d\d|train \d/);
  });

  it('packets without a measurement are counted as failed runs, not read as agreement', () => {
    expect(markup).toContain('unmeasured');
    expect(markup).toContain('none carries a measurement');
  });

  it('the measured header names head, at, and the admitted / compared / adrift counts', () => {
    for (const field of ['measured.head', 'measured.at', 'measured.live_admitted', 'measured.fields_compared', 'drift.counts.fields']) {
      expect(markup, `the header reads ${field}`).toContain(field);
    }
    // A null head is said with the row's reason, never rendered as a blank sha.
    expect(markup).toContain('head_why');
  });

  it('the section-00 header links the measured packet: the record is one click away', () => {
    // 0646ac7d: newest.id used to reach only the approve body.
    const header = markup.indexOf('00 — THE MEASUREMENT');
    expect(header).toBeGreaterThan(-1);
    const line = markup.slice(header, markup.indexOf('</div>', header));
    expect(line).toContain('href={`/jobs/${newest.id}`}');
  });

  it('the header draws the measurement age and marks it past the daily period', () => {
    // d83886c6: a stopped cadence would otherwise look current.
    expect(src).toContain('measurementAge(newest.measured.at, now)');
    const header = markup.indexOf('00 — THE MEASUREMENT');
    const line = markup.slice(header, markup.indexOf('</div>', header));
    expect(line).toContain('age.text');
    expect(line).toContain('class:warn={age.stale}');
    expect(line).toContain('STALE_AFTER_HOURS');
  });

  it('the compared fields are the packet\'s own statement, and the subtitle no longer restates them', () => {
    // fb5f1c2f: the subtitle said "label, description, category" and had
    // fallen behind the script's four step facets.
    expect(markup).toContain('newest.measured.method_fields');
    const header = src.slice(src.indexOf('<PageHeader'), src.indexOf('/>', src.indexOf('<PageHeader')));
    expect(header).not.toContain('label, description, category');
  });

  it('the tenant seeds counted in "authored" are said in one line under the strip', () => {
    // 90a24e28: "admitted 64 · authored 96" read as 32 missing kinds.
    const strip = markup.indexOf('class="pd-strip"');
    const tenants = markup.indexOf('newest.drift.tenants');
    expect(tenants).toBeGreaterThan(strip);
    expect(markup).toContain('not admitted here');
    expect(markup).toContain('does not run the demo tenant, by design');
  });

  it('draws one row per drifted field: kind, field, live version, both excerpts', () => {
    const rows = markup.indexOf('{#each newest.drift.fields as');
    expect(rows).toBeGreaterThan(-1);
    const row = markup.slice(rows, markup.indexOf('{/each}', rows));
    for (const cell of ['.kind', '.field', '.live_version', '.tree_window', '.live_window', '.at']) {
      expect(row, `the drift row carries ${cell}`).toContain(cell);
    }
    // The other two directions the script names are drawn, not dropped.
    expect(markup).toContain('drift.unauthored');
    expect(markup).toContain('drift.pending');
  });
});

// Car 3b: the approve. One control per adrift kind, reading the verb's
// own answers; the force path two clicks from the plain one.
describe('the drift page approves a publish through the ops-request packet', () => {
  const page = readFileSync(join(import.meta.dir, 'ProtocolDriftPage.svelte'), 'utf8');
  const control = readFileSync(join(import.meta.dir, 'ApprovePublish.svelte'), 'utf8');
  const markup = control.slice(control.indexOf('</script>'));

  it('the page groups the drift rows by kind and hands each its latest request and the execute role', () => {
    expect(page).toContain("from './approve'");
    for (const call of ['adriftKinds(', 'latestFor(', 'loadPublishRequests(', 'loadExecuteAuthorityRole(', '<ApprovePublish']) {
      expect(page, `the page uses ${call}`).toContain(call);
    }
    // The viewer is the actor: id and role come from the session, never typed.
    expect(page).toContain('session.value.user.id');
    expect(page).toContain('session.value.user.role');
  });

  it('a failed requests read withholds the controls with the failure marker, never offers them blind', () => {
    const failed = page.indexOf("requests.kind === 'failed'");
    expect(failed).toBeGreaterThan(-1);
    const arm = page.slice(failed, page.indexOf('{:else', failed));
    expect(arm).toContain('load-failed');
    expect(arm).not.toContain('<ApprovePublish');
  });

  // cece5515 (a): the authority-role read failing withholds the
  // controls with the failure marker, as the requests read does.
  it('a failed authority-role read withholds the controls with the failure marker, never admits blind', () => {
    const failed = page.indexOf("authorityRole.kind === 'failed'");
    expect(failed).toBeGreaterThan(-1);
    const arm = page.slice(failed, page.indexOf('{:else', failed));
    expect(arm).toContain('load-failed');
    expect(arm).toContain('The ops-request protocol did not answer: {authorityRole.error}');
    expect(arm).not.toContain('<ApprovePublish');
  });

  // cece5515 (b): a refused write is said beside the control, in the
  // failure marker, with the thrown message fileApprove built.
  it('a refused write renders "The request was not filed" with the error, never swallowed', () => {
    expect(control).toMatch(/catch \(e\) \{\s*error = e instanceof Error \? e\.message : String\(e\);/);
    expect(markup).toContain('<p class="load-failed ap-error">The request was not filed: {error}</p>');
  });

  // cece5515 (c): the 10 s re-read runs only while a request is open,
  // and stops once none is. The behaviour is also driven in
  // protocol-drift-page.mocked.spec.ts under a fake clock.
  it('the re-read polls only while a request is open, and clears when none is', () => {
    const effect = page.slice(page.indexOf('$effect(() => {'), page.indexOf('});', page.indexOf('$effect(() => {')));
    expect(effect).toContain("requests.kind === 'ready' && requests.data.some((r) => r.status === 'open')");
    expect(effect).toContain('if (inFlight && poll === null) poll = setInterval(');
    expect(effect).toContain('if (!inFlight && poll !== null) {');
    expect(effect).toContain('clearInterval(poll)');
    expect(page).toContain('const POLL_MS = 10_000;');
  });

  it('the control files through approveBody + fileApprove, inside the write gate, gated by the execute role', () => {
    expect(control).toContain('approveBody(');
    expect(control).toContain('fileApprove(');
    expect(control).toContain('approveAuthority(');
    expect(markup).toContain('<WriteGate>');
    // The refusal names the role in the disabled control's title, as the Abort control does.
    expect(markup).toContain('disabled={refusal !== null');
  });

  it('the force path exists only in the force mode and enables only on the typed field name', () => {
    const force = markup.indexOf("mode.kind === 'force'");
    expect(force).toBeGreaterThan(-1);
    const arm = markup.slice(force, markup.indexOf('{:else}', force));
    expect(arm).toContain("file('force')");
    expect(arm).toContain('forceConfirmed(typed, fieldNames)');
    expect(arm).toContain('erase');
    // Nowhere else files with force.
    expect(markup.slice(0, force)).not.toContain("file('force')");
    expect(markup.slice(markup.indexOf('{:else}', force))).not.toContain("file('force')");
  });

  it('the answer is the packet link plus exit code and the FULL output, never a tail', () => {
    expect(markup).toContain('href={`/jobs/${latest.id}`}');
    expect(markup).toContain('latest.exit_code');
    expect(markup).toContain('<pre class="ap-output">{latest.output}</pre>');
    expect(control).not.toMatch(/output\.slice\(|output\.split\(/);
  });
});

// Car 3c: a row the verb published since the measurement reads
// superseded — greyed, labelled with the versions and the instant, and
// out of the header's adrift count — until the next 05:20 run
// re-measures it. The decision is rowState's (approve.test.ts); this
// pins that the page draws it in all three places, from the requests
// it already reads, with no new fetch.
describe('a published drift row reads superseded until the next measurement', () => {
  const page = readFileSync(join(import.meta.dir, 'ProtocolDriftPage.svelte'), 'utf8');
  const control = readFileSync(join(import.meta.dir, 'ApprovePublish.svelte'), 'utf8');
  const markup = page.slice(page.indexOf('</script>'));

  it('the page decides each kind through rowState off latestFor and the packet measured.at, no new fetch', () => {
    expect(page).toContain('rowState(');
    expect(page).toContain('supersededFieldCount(');
    expect(page).toContain('newest.measured.at');
    expect((page.match(/fetchRemote\(|loadPublishRequests\(/g) ?? []).length).toBe(1);
  });

  it('the header subtracts the superseded fields and says how many were published since', () => {
    const adrift = markup.indexOf('title="compared fields where the file and the live row disagree');
    expect(adrift).toBeGreaterThan(-1);
    const cell = markup.slice(adrift, markup.indexOf('</div>\n      </div>', adrift));
    expect(cell).toContain('supersededFields');
    expect(cell).toContain('published since');
  });

  it('a drift row of a superseded kind is greyed in the table', () => {
    const rows = markup.indexOf('{#each newest.drift.fields as');
    const row = markup.slice(rows, markup.indexOf('{/each}', rows));
    expect(row).toContain('class:superseded=');
  });

  it('the control is greyed and labelled published vN→vM at <time>, re-measured at the next drift run', () => {
    expect(page).toContain('standing={');
    expect(control).toContain("standing.kind === 'superseded'");
    expect(control).toContain('re-measured at the next drift run');
    expect(control).toContain('class:ap-superseded=');
    // The label names both versions from the verb's own line, or says they were not named.
    expect(control).toContain('standing.from');
    expect(control).toContain('standing.to');
  });
});
