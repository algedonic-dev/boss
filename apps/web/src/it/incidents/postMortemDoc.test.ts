// The semi-structured post-mortem renderer, pinned at the data layer.
//
// Incident packets carry their findings as free-form Job
// metadata, and the shape has already drifted between the two live
// packets: the 2026-08-22 one carries incident_at / summary /
// mitigations_shipped / open_questions / evidence, the 2026-08-13 one
// carries ask / declared_by / incident_date / outcome. The renderer's
// contract is therefore SEMI-structured: keys it knows get first-class
// sections in a fixed reading order, keys it does not know render as
// labeled prose blocks after them — never dropped, never dumped as raw
// JSON.

import { describe, expect, test } from 'bun:test';
import {
  bodyParts,
  closedOutcome,
  humanizeKey,
  incidentAt,
  postMortemSections,
  stepFindings,
} from './postMortemDoc';
import type { Step } from '../../jobs/types';

describe('postMortemSections — well-known keys in reading order', () => {
  test('orders the known sections canonically, whatever order the metadata carries them in', () => {
    const sections = postMortemSections({
      evidence: 'readyz verbose captured',
      open_questions: '(a) dedicated etcd disk',
      mitigations_shipped: 'gate-run protocol v1 ACTIVE',
      summary: 'COMPLETE TIMELINE.',
      incident_at: '2026-08-22, two windows',
    });
    expect(sections.map((s) => s.label)).toEqual([
      'When it happened',
      'Summary',
      'Mitigations shipped',
      'Open questions',
      'Evidence',
    ]);
  });

  test('timeline and root_cause slot between summary and the mitigations family', () => {
    const sections = postMortemSections({
      root_cause: 'an unbounded dev container',
      mitigations_shipped: 'throttles baked in',
      timeline: '16:38Z first casualty',
      summary: 'the short version',
    });
    expect(sections.map((s) => s.key)).toEqual([
      'summary',
      'timeline',
      'root_cause',
      'mitigations_shipped',
    ]);
  });

  test('every mitigations* key is first-class, kept in authored order within the family', () => {
    const sections = postMortemSections({
      open_questions: 'still open',
      mitigations_shipped: 'shipped',
      mitigation_next: 'planned',
    });
    expect(sections.map((s) => s.key)).toEqual([
      'mitigations_shipped',
      'mitigation_next',
      'open_questions',
    ]);
  });

  test('the older packet shape (incident_date) still gets the When section first', () => {
    const sections = postMortemSections({
      ask: 'Please write the post mortem.',
      incident_date: '2026-08-13',
    });
    expect(sections[0]).toEqual({
      key: 'incident_date',
      label: 'When it happened',
      body: '2026-08-13',
    });
  });
});

describe('postMortemSections — unknown keys are labeled prose, never dropped', () => {
  test('unknown string keys render after the known sections, in authored order', () => {
    const sections = postMortemSections({
      ask: 'Do we have a good surface for post mortems?',
      declared_by: 'emp-david',
      summary: 'the incident',
    });
    expect(sections.map((s) => s.label)).toEqual(['Summary', 'Ask', 'Declared by']);
    expect(sections[1]?.body).toBe('Do we have a good surface for post mortems?');
  });

  test('non-string values are preserved as prose, not dropped and not raw JSON', () => {
    const sections = postMortemSections({
      retries: 3,
      self_inflicted: true,
      affected_nodes: ['cp-2', 'cp-3'],
      window: { start: '18:10Z', end: '18:35Z' },
    });
    const byKey = new Map(sections.map((s) => [s.key, s.body]));
    expect(byKey.get('retries')).toBe('3');
    expect(byKey.get('self_inflicted')).toBe('true');
    expect(byKey.get('affected_nodes')).toBe('cp-2\ncp-3');
    // A flat object renders as labeled lines — no braces, no quotes.
    expect(byKey.get('window')).toBe('Start: 18:10Z\nEnd: 18:35Z');
  });

  test('blank and null values are omitted rather than rendered as empty sections', () => {
    expect(
      postMortemSections({ summary: '   ', evidence: '', gone: null, list: [] }),
    ).toEqual([]);
  });

  test('the omit list keeps keys the caller renders elsewhere out of the body', () => {
    const sections = postMortemSections(
      { outcome: 'shipped', summary: 'the incident', incident_date: '2026-08-13' },
      ['outcome', 'incident_date'],
    );
    expect(sections.map((s) => s.key)).toEqual(['summary']);
  });
});

describe('humanizeKey', () => {
  test('spells snake_case and kebab-case keys as sentence-cased labels', () => {
    expect(humanizeKey('declared_by')).toBe('Declared by');
    expect(humanizeKey('mitigations-shipped')).toBe('Mitigations shipped');
    expect(humanizeKey('ask')).toBe('Ask');
  });
});

describe('incidentAt', () => {
  test('prefers incident_at, falls back to the older incident_date, else null', () => {
    expect(incidentAt({ incident_at: '2026-08-22', incident_date: '2026-08-13' })).toBe(
      '2026-08-22',
    );
    expect(incidentAt({ incident_date: '2026-08-13' })).toBe('2026-08-13');
    expect(incidentAt({})).toBeNull();
    expect(incidentAt({ incident_at: '  ' })).toBeNull();
  });
});

describe('closedOutcome', () => {
  const step = (over: Partial<Step>): Step => ({
    id: 's',
    job_id: 'j',
    kind: 'task',
    title: 'a step',
    assignee_id: null,
    status: 'pending',
    sort_order: 0,
    blocked_by: [],
    completed_on: null,
    metadata: {},
    ...over,
  });

  test('reads the last completed transition — the terminal that actually fired', () => {
    const outcome = closedOutcome({
      metadata: {},
      steps: [
        step({ title: 'Incident opened', status: 'completed', sort_order: 0 }),
        step({ title: 'Post-mortem closed', status: 'completed', sort_order: 7 }),
        step({ title: 'Superseded by another post-mortem', status: 'skipped', sort_order: 8 }),
      ],
    });
    expect(outcome).toBe('Post-mortem closed');
  });

  test('falls back to a metadata.outcome string when steps are absent', () => {
    expect(closedOutcome({ metadata: { outcome: 'All mitigations shipped' } })).toBe(
      'All mitigations shipped',
    );
    expect(closedOutcome({ metadata: {} })).toBeNull();
  });
});

// Gap 1 of page-audit 9b9849f5 (backlog 434b6e0d). In incident v3 every
// forcing-question answer is STEP metadata: the first closed v3 packet,
// a65ba21e, carried 7 job keys and 23 step finding keys, and the archive
// read only the 7. These pin the step half the archive now renders, the
// way infra/step-plugins/incident-review.js renders it for the reviewer.
describe('stepFindings — what each terminal step found, in workflow order', () => {
  const step = (over: Partial<Step>): Step => ({
    id: 's',
    job_id: 'j',
    kind: 'task',
    title: 'a step',
    assignee_id: null,
    status: 'completed',
    sort_order: 0,
    blocked_by: [],
    completed_on: '2026-09-26',
    metadata: {},
    ...over,
  });

  test('labels each finding by its field, steps in sort order, plumbing filtered', () => {
    const found = stepFindings([
      step({
        id: 'cause',
        title: 'What caused it',
        sort_order: 4,
        metadata: { authority_role: 'platform-admin', why_now: 'a token rotated', root_cause: 'a mirror force-synced' },
      }),
      step({
        id: 'timeline',
        title: 'Establish the timeline from evidence',
        sort_order: 2,
        metadata: { authority_role: 'platform-admin', detected_at: '2026-09-26T01:10Z', timeline: '01:00 push; 01:10 noticed' },
      }),
    ]);
    expect(found.map((f) => f.step.id)).toEqual(['timeline', 'cause']);
    // Known keys first, in the reading order postMortemSections keeps.
    expect(found.map((f) => f.sections.map((s) => s.label))).toEqual([
      ['Timeline', 'Detected at'],
      ['Root cause', 'Why now'],
    ]);
    expect(found.flatMap((f) => f.sections.map((s) => s.key))).not.toContain('authority_role');
  });

  test('a step that recorded only plumbing, and a step not yet terminal, render nothing', () => {
    const found = stepFindings([
      step({ id: 'raised', kind: 'trigger', metadata: { trigger_kind: 'operator', trigger_name: 'raise' } }),
      step({ id: 'review', kind: 'incident-review', sort_order: 8, metadata: { authority_role: 'platform-admin', human_only: true } }),
      step({ id: 'closed', kind: 'outcome', sort_order: 9, metadata: { outcome_kind: 'completed' } }),
      step({ id: 'later', status: 'ready', sort_order: 3, metadata: { timeline: 'not recorded yet' } }),
    ]);
    expect(found).toEqual([]);
  });

  test('a skipped step still shows what it recorded, as the review surface does', () => {
    const found = stepFindings([step({ status: 'skipped', metadata: { prevention: 'none needed' } })]);
    expect(found.map((f) => f.sections.map((s) => s.body))).toEqual([['none needed']]);
  });

  test('no steps on the read is no findings, not a throw', () => {
    expect(stepFindings(undefined)).toEqual([]);
  });
});

// "action_packets render as text" (434b6e0d): a packet ref in a field
// that names packets becomes a link. Only such fields — a bare
// 8-hex word elsewhere may be a commit, and a commit linked as a
// packet is a click that dead-ends.
describe('bodyParts — packet refs in a packet field become links', () => {
  test('links the short id that leads each line, and a full uuid anywhere', () => {
    const parts = bodyParts(
      'action_packets',
      '21d54f4a remove the push mirror\nan item to add: a rehearsal\ncarried by e6406701-a8cc-44da-a111-9670aff620db',
    );
    expect(parts.filter((p) => p.ref !== null).map((p) => p.ref)).toEqual([
      '21d54f4a',
      'e6406701-a8cc-44da-a111-9670aff620db',
    ]);
    // Nothing is dropped: the parts spell the body back exactly.
    expect(parts.map((p) => p.text).join('')).toBe(
      '21d54f4a remove the push mirror\nan item to add: a rehearsal\ncarried by e6406701-a8cc-44da-a111-9670aff620db',
    );
  });

  test('a field that does not name packets links nothing', () => {
    expect(bodyParts('shipped_in_this_incident', 'd812f1b7 the fix').map((p) => p.ref)).toEqual([null]);
  });

  test('a longer hex run (a commit sha) is not read as a short packet id', () => {
    const sha = 'd812f1b7a0c94e2f8b1d3c5e7f9a0b2c4d6e8f01';
    expect(bodyParts('follow_up_packets', sha).map((p) => p.ref)).toEqual([null]);
  });
});
