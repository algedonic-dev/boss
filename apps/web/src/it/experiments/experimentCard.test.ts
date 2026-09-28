// What an experiment's card says, pinned at the data layer (page-audit
// ec8351f4, gaps 1 and 2). The Concluded card showed Measured as
// control · candidate · n and Decided as the decision alone, dropping
// the three fields that say how far a result can be trusted —
// `measure.source`, `state.sample_floor`, `decide.against_stated_rule`
// (backlog 3633a918). And an abandoned packet, which closes with no
// `measure` or `decide` at all, rendered as a row of dashes instead of
// the `reason` its terminal requires (backlog baf0c973).

import { describe, expect, test } from 'bun:test';
import { armOf, concludedCard, measuredLine } from './experimentCard';
import type { Job, Step } from '../../jobs/types';

const step = (slug: string, metadata: Record<string, unknown>, sort_order: number): Step => ({
  id: `s-${slug}`,
  job_id: 'j',
  kind: 'task',
  title: slug,
  assignee_id: null,
  status: 'completed',
  sort_order,
  blocked_by: [],
  completed_on: '2026-09-27',
  metadata,
  spec_slug: slug,
});

const job = (steps: ReadonlyArray<Step>, metadata: Record<string, unknown> = {}): Job =>
  ({
    id: 'j',
    kind: 'protocol-experiment',
    subject: { subject_kind: 'custom', id: 'x' },
    title: 'Experiment: gate width',
    owner_id: 'emp-david',
    status: 'closed',
    priority: 'standard',
    opened_on: '2026-09-20',
    due_on: null,
    closed_on: '2026-09-27',
    tags: [],
    metadata,
    steps: [...steps],
  }) as unknown as Job;

const STATE = step(
  'state',
  {
    hypothesis: 'A 6-wide gate is faster than a 4-wide one',
    metric: 'gate wall clock',
    control: '4 jobs',
    candidate: '6 jobs',
    decision_rule: 'candidate at least 10% faster',
    sample_floor: '5 gates per arm',
  },
  1,
);
const MEASURE = step(
  'measure',
  { control_result: '11m', candidate_result: '8m', samples: '6', source: 'gate receipts' },
  3,
);
const DECIDE = step(
  'decide',
  {
    verdict: 'promote',
    decision: 'promote the 6-wide gate',
    against_stated_rule: 'met: 27% faster, over the 10% the rule named',
    confounds: 'one warm target',
    elapsed: '40m',
  },
  4,
);

describe('a decided experiment', () => {
  const card = concludedCard(job([DECIDE, STATE, MEASURE], { outcome: 'promoted' }));

  test('carries the source and the floor beside n', () => {
    expect(card.kind).toBe('decided');
    if (card.kind !== 'decided') return;
    expect(card.measured.source).toBe('gate receipts');
    expect(card.measured.floor).toBe('5 gates per arm');
    expect(measuredLine(card.measured)).toBe(
      'control 11m · candidate 8m · n=6 (floor 5 gates per arm) · source: gate receipts',
    );
  });

  test('carries the decision against the rule stated before the result', () => {
    if (card.kind !== 'decided') throw new Error(`expected decided, got ${card.kind}`);
    expect(card.decision).toBe('promote the 6-wide gate');
    expect(card.againstRule).toBe('met: 27% faster, over the 10% the rule named');
    expect(card.confounds).toBe('one warm target');
    expect(card.predicted).toBe('A 6-wide gate is faster than a 4-wide one');
  });

  test('an unrecorded field reads as a dash or a question, never as blank', () => {
    const bare = concludedCard(job([STATE], { outcome: 'inconclusive' }));
    if (bare.kind !== 'decided') throw new Error(`expected decided, got ${bare.kind}`);
    expect(measuredLine(bare.measured)).toBe('control — · candidate — · n=? (floor 5 gates per arm) · source: —');
    expect(bare.decision).toBe('');
    expect(bare.againstRule).toBe('');
  });
});

describe('an abandoned experiment', () => {
  test('reads its terminal reason, not the measure and decide it never reached', () => {
    const card = concludedCard(
      job([STATE, step('abandoned', { reason: 'the gate pool was resized mid-run' }, 7)], {
        outcome: 'abandoned',
        abandoned: 'true',
      }),
    );
    expect(card).toEqual({
      kind: 'abandoned',
      predicted: 'A 6-wide gate is faster than a 4-wide one',
      reason: 'the gate pool was resized mid-run',
    });
  });
});

describe('the arms', () => {
  test('read the v5 free-text fields first', () => {
    expect(armOf(job([STATE]), 'control')).toBe('4 jobs');
    expect(armOf(job([STATE]), 'candidate')).toBe('6 jobs');
  });

  test('fall back to *_version on a packet stated before v5 widened them', () => {
    const old = job([step('state', { control_version: '3', candidate_version: '4' }, 1)]);
    expect(armOf(old, 'control')).toBe('3');
    expect(armOf(old, 'candidate')).toBe('4');
  });
});
