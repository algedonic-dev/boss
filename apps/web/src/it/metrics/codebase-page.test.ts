import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

// The codebase page reads the daily `maintenance-codebase-metrics`
// packets. This pins its three empty states to the template the way
// drift-page.test.ts pins the Drift tab's: a read that FAILED, a
// cadence that has not filed yet (zero packets, a successful read),
// and packets that exist with no measurement are three different
// facts. Until 2026-09-18 the second wore `.load-failed` — the ONE
// class that says "this read failed" (tests/mocked/_routes.ts) — on a
// read that had answered 200 and `[]`, which the interaction crawl's
// empty-backend leg reported on /it/codebase and /it/design/codebase
// (car f2b8a01c, backlog 409feafd).
describe('the codebase page tells its three empty states apart', () => {
  const src = readFileSync(join(import.meta.dir, 'CodebaseTrendPage.svelte'), 'utf8');
  // Whitespace collapsed: a phrase that wraps in the source is still
  // one phrase on the page.
  const markup = src.slice(src.indexOf('</script>')).replace(/\s+/g, ' ');

  const arm = (marker: string): string => {
    const at = markup.indexOf(marker);
    expect(at, `the template has an arm for ${marker}`).toBeGreaterThan(-1);
    return markup.slice(at, markup.indexOf('{:else', at));
  };

  it('a failed read carries the shared failure marker', () => {
    const failed = arm("page.kind === 'failed'");
    expect(failed).toContain('load-failed');
    expect(failed).toContain('{page.error}');
  });

  it('no packet yet is an honest empty, not a failed read', () => {
    const none = arm('ready.total === 0');
    expect(none).toContain('has not filed');
    expect(none).not.toContain('load-failed');
  });

  it('packets without a measurement are counted as failed runs, and say so', () => {
    const unmeasured = arm('!newest');
    expect(unmeasured).toContain('unmeasured');
    expect(unmeasured).toContain('none carries a measurement');
    expect(unmeasured).not.toContain('has not filed');
  });

  // a91a39a4 (page audit f82b05a9, gap 2): the heading said "THE
  // CODEBASE NOW" over a row eleven hours old, and a failed newest run
  // showed only as a count in the footnote. The rendered behaviour is
  // pinned in tests/mocked/codebase-page.mocked.spec.ts; this holds the
  // template to reading it from the record rather than typing it.
  it('the 00 heading states the measurement age against the reader clock, and never says NOW', () => {
    expect(src).toContain('measurementAge(newest.measured.at, now)');
    expect(markup).not.toContain('THE CODEBASE NOW');
    const heading = markup.slice(markup.indexOf('00 — THE CODEBASE'), markup.indexOf('ct-stats'));
    expect(heading).toContain('{age.text} ago');
    expect(heading).toContain('STALE_AFTER_HOURS');
  });

  it('a newest packet that is not the measured one is named with its outcome, result and link', () => {
    expect(src).toContain('unmeasuredNewest(');
    const at = markup.indexOf('{#if newer}');
    expect(at, 'the template has an arm for a newer unmeasured packet').toBeGreaterThan(-1);
    const note = markup.slice(at, markup.indexOf('{/if}', at));
    for (const field of ['newer.outcome', 'newer.result', '/jobs/${newer.id}', 'newer.title']) {
      expect(note, `the note reads ${field}`).toContain(field);
    }
    // The read succeeded; a failed RUN is not a failed read, so it does
    // not wear the one class that says so (tests/mocked/_routes.ts).
    expect(note).not.toContain('load-failed');
  });

  // 818cd10c: /it/design/codebase was retired with no alias (car N3 of
  // design e765b3fc); the page no longer names it as a second home.
  it('names no retired route as its own', () => {
    expect(src).not.toContain('/it/design/codebase');
  });
});

// ad86e359 (page audit f82b05a9, gap 4): the page has two reads, and
// until this block only the metrics read's arms were pinned. The outage
// crawl's one-marker-per-route check is satisfied by the metrics arm
// alone, so the Surfaces section's failure arm and its empty arm could
// regress with nothing going red. Same source-template pin, second read.
describe('the surfaces section tells a failed read from an empty week', () => {
  const src = readFileSync(join(import.meta.dir, 'SurfaceUsage.svelte'), 'utf8');
  const markup = src.slice(src.indexOf('</script>')).replace(/\s+/g, ' ');

  const arm = (marker: string): string => {
    const at = markup.indexOf(marker);
    expect(at, `the template has an arm for ${marker}`).toBeGreaterThan(-1);
    return markup.slice(at, markup.indexOf('{:else', at));
  };

  it('a failed read carries the shared failure marker and its error', () => {
    const failed = arm("rollup.kind === 'failed'");
    expect(failed).toContain('load-failed');
    expect(failed).toContain('{rollup.error}');
    expect(failed).toContain('is not a quiet week');
  });

  it('an empty roll-up is a window that has not filled, not a failed read and not a verdict', () => {
    const empty = arm('actors.length === 0');
    expect(empty).toContain('No surface open is recorded');
    expect(empty).toContain('a window that has not filled, not a verdict');
    expect(empty).not.toContain('load-failed');
  });

  // 13ded76c part (a): the candidates are the catalog minus the opens,
  // so the list's scope is the catalog's, and the section says so with
  // the count, and lists the opened patterns outside it on their own.
  it('states the roster is the nav catalog, with its count, and lists opened-but-uncatalogued routes', () => {
    const at = markup.indexOf('the deletion candidates');
    expect(at).toBeGreaterThan(-1);
    const rest = markup.slice(at);
    expect(rest).toContain('The roster is the nav catalog');
    expect(rest).toContain('{roster.length}');
    expect(src).toContain('uncatalogued(ready.rows, roster)');
    expect(rest).toContain('su-uncatalogued');
  });
});
