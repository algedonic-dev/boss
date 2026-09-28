// The job page reads every operator annotation on a packet (backlog
// 1abb9928, 2026-09-27). Until this, JobDetailPage read job.metadata
// only for the keys a workflow declares as link fields, so a plan of
// action, an ETA, a needs_david line or a recorded decision written
// through PATCH /api/jobs/{id}/metadata reached David only through the
// API. The decisions, pinned as pure functions:
//   order    — the alarm keys first, in the order the normal-operations
//              rule reads them, then decided_*, then the rest by name.
//   shape    — prose (a known prose key, or any long or multi-line
//              string) as a paragraph; a short scalar as a row; a list
//              of scalars as a list; anything else as pretty JSON.
//   skip     — only what the page already renders elsewhere: the
//              declared link fields and the corrections list.
//   ids      — a uuid inside any string is a link, the rest is text.
import { describe, expect, test } from 'bun:test';

import {
  ALARM_KEYS,
  classifyAnnotation,
  idSegments,
  jobAnnotations,
} from './annotations';

const PLAN = 'Step 1: rebase the car.\nStep 2: re-gate with the park flags.';
const CAR_A = '1abb9928-67d2-4aa1-a950-f8049f88b8fa';
const CAR_B = 'd6267b8e-c0f6-4dd3-b8eb-426482191505';
const LANDED = [CAR_A, CAR_B];

describe('jobAnnotations — order', () => {
  test('alarm keys first in their own order, then decided_*, then the rest by name', () => {
    const keys = jobAnnotations({
      zeta: 'z',
      remaining: 'two cars',
      'decided_2026-09-27_david': 'build it',
      steps_for_david: 'sign the release',
      area: 'web/jobs',
      needs_david: 'yes',
      'decided_2026-09-26_triage': 'route build',
      eta_utc: '2026-09-28T02:00:00Z',
      plan_of_action: PLAN,
      owner: 'agent-claude',
    }, new Set()).map((a) => a.key);
    expect(keys).toEqual([
      'owner',
      'plan_of_action',
      'eta_utc',
      'needs_david',
      'steps_for_david',
      'decided_2026-09-26_triage',
      'decided_2026-09-27_david',
      'area',
      'remaining',
      'zeta',
    ]);
  });

  test('the alarm key list is the one the normal-operations rule names', () => {
    expect([...ALARM_KEYS]).toEqual(['owner', 'plan_of_action', 'eta_utc', 'needs_david', 'steps_for_david']);
  });

  test('an absent or empty metadata answers no annotations', () => {
    expect(jobAnnotations(undefined, new Set())).toEqual([]);
    expect(jobAnnotations({}, new Set())).toEqual([]);
  });
});

describe('jobAnnotations — skip', () => {
  test('the declared link fields and the corrections list are skipped, nothing else', () => {
    const keys = jobAnnotations({
      related_car: CAR_A,
      corrections: [{ step: 'triage', field: 'evidence' }],
      owner: 'agent-claude',
      description: 'why this exists',
    }, new Set(['related_car'])).map((a) => a.key);
    expect(keys).toEqual(['owner', 'description']);
  });
});

describe('classifyAnnotation — shape', () => {
  test('a known prose key is a paragraph even when short', () => {
    for (const k of ['plan_of_action', 'steps_for_david', 'needs_david', 'description', 'decided_2026-09-27_david']) {
      expect(classifyAnnotation(k, 'short').kind).toBe('prose');
    }
  });

  test('any string with a newline, or longer than the threshold, is a paragraph', () => {
    expect(classifyAnnotation('area', 'one\ntwo')).toEqual({ kind: 'prose', text: 'one\ntwo' });
    expect(classifyAnnotation('area', 'x'.repeat(121)).kind).toBe('prose');
    expect(classifyAnnotation('area', 'x'.repeat(120)).kind).toBe('text');
  });

  test('a short scalar is a text row, stringified as written', () => {
    expect(classifyAnnotation('eta_utc', '2026-09-28T02:00:00Z')).toEqual({ kind: 'text', text: '2026-09-28T02:00:00Z' });
    expect(classifyAnnotation('attempts', 3)).toEqual({ kind: 'text', text: '3' });
    expect(classifyAnnotation('needs_david', true)).toEqual({ kind: 'text', text: 'true' });
    expect(classifyAnnotation('owner', null)).toEqual({ kind: 'text', text: 'null' });
  });

  test('a list of scalars is a list; a list of ids stays a list of ids', () => {
    expect(classifyAnnotation('landed_cars', LANDED)).toEqual({ kind: 'list', items: LANDED });
    expect(classifyAnnotation('remaining', ['a', 2, false])).toEqual({ kind: 'list', items: ['a', '2', 'false'] });
  });

  test('an object, or a list holding one, is pretty JSON — every key readable', () => {
    const v = { verdict: 'approve', by: 'emp-david' };
    expect(classifyAnnotation('review_verdict', v)).toEqual({ kind: 'structured', json: JSON.stringify(v, null, 2) });
    const mixed = [{ a: 1 }, 'b'];
    expect(classifyAnnotation('remaining', mixed)).toEqual({ kind: 'structured', json: JSON.stringify(mixed, null, 2) });
  });
});

describe('idSegments — ids are links, the rest is text', () => {
  test('a bare uuid is one id segment', () => {
    expect(idSegments(CAR_A)).toEqual([{ kind: 'id', id: CAR_A }]);
  });

  test('uuids inside prose split it without losing a character', () => {
    const text = `landed on ${CAR_A}, then ${CAR_B}.\nDone`;
    const segs = idSegments(text);
    expect(segs).toEqual([
      { kind: 'text', text: 'landed on ' },
      { kind: 'id', id: CAR_A },
      { kind: 'text', text: ', then ' },
      { kind: 'id', id: CAR_B },
      { kind: 'text', text: '.\nDone' },
    ]);
    expect(segs.map((s) => (s.kind === 'id' ? s.id : s.text)).join('')).toBe(text);
  });

  test('a short id is not a uuid and stays text', () => {
    expect(idSegments('car 817b1b84')).toEqual([{ kind: 'text', text: 'car 817b1b84' }]);
  });
});
