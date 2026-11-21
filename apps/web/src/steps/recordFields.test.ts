// A declared field is shown by what it holds (backlog 7ac7cd73). The
// generic surface drew EVERY declared field as a text box seeded with
// the value only when it was a string, so a declared object field — a
// car's `release` record {sha, review, verdict, by, at, main_sha} —
// rendered as an EMPTY box over the record that was there, and a field a
// door writes (`hold_sha`) became something a hand could type into. A
// Workflow row therefore could not declare what its doors write without
// the page lying about it.

import { describe, expect, test } from 'bun:test';
import type { StepField } from '../jobs/types';
import {
  editableValues,
  editedAnswers,
  followRecord,
  missingRecords,
  readOnlyField,
  recordText,
} from './recordFields';

const field = (name: string, field_type: string, extra: Partial<StepField> = {}): StepField => ({
  name,
  field_type,
  ...extra,
});

describe('readOnlyField', () => {
  test('an object or array field is a record, never a text box', () => {
    expect(readOnlyField(field('release', 'object'), undefined)).toBe(true);
    expect(readOnlyField(field('questions', 'array'), [])).toBe(true);
  });

  test('a field holding a structured value is shown as that value, whatever its type says', () => {
    expect(readOnlyField(field('release', 'string'), { sha: 'abc' })).toBe(true);
    expect(readOnlyField(field('items', 'string'), ['a'])).toBe(true);
  });

  test('a field the row names a writer for is the door\'s, not a hand\'s', () => {
    expect(readOnlyField(field('hold_sha', 'string', { writer: 'runner:conductor' }), 'abc')).toBe(true);
    expect(readOnlyField(field('hold_sha', 'string', { writer: 'runner:conductor' }), undefined)).toBe(true);
  });

  test('a plain string field stays an answer the executor types', () => {
    expect(readOnlyField(field('evidence', 'string'), 'seen')).toBe(false);
    expect(readOnlyField(field('evidence', 'string'), undefined)).toBe(false);
    expect(readOnlyField(field('route', 'build|design'), null)).toBe(false);
  });
});

describe('recordText', () => {
  test('a structured value renders as its whole record', () => {
    const release = { sha: 'abc123', verdict: 'release', by: 'emp-001' };
    expect(recordText(release)).toBe(JSON.stringify(release, null, 2));
  });

  test('a string renders as itself and an absent value as null', () => {
    expect(recordText('abc123')).toBe('abc123');
    expect(recordText(undefined)).toBeNull();
    expect(recordText(null)).toBeNull();
  });
});

describe('editableValues', () => {
  test('a read-only field is never part of what the surface writes back', () => {
    const fields = [field('release', 'object'), field('evidence', 'string')];
    const values = editableValues(fields, { release: { sha: 'abc' }, evidence: 'seen' });
    expect(values).toEqual({ evidence: 'seen' });
  });
});

describe('missingRecords', () => {
  test('a required record that is present is not missing; one that is absent is', () => {
    const fields = [
      field('release', 'object', { required: true }),
      field('hold_sha', 'string', { required: true, writer: 'runner:conductor' }),
      field('evidence', 'string', { required: true }),
    ];
    expect(missingRecords(fields, { release: { sha: 'abc' } })).toEqual(['hold_sha']);
  });
});

// A page left open re-sent every box it was drawn with (backlog
// ec1e1980): an answer is what the operator EDITED here, measured against
// what the page last knew the record held — never every box it shows.
describe('editedAnswers', () => {
  test('an untouched box is not an answer, however it was seeded', () => {
    const values = { finding: 'as first drawn', note: '' };
    expect(editedAnswers(values, { ...values }, new Set())).toEqual({});
  });

  test('an edited box is, and only that one', () => {
    const baseline = { finding: 'as first drawn', note: '' };
    const values = { finding: 'as first drawn', note: 'checked the fixture' };
    expect(editedAnswers(values, baseline, new Set())).toEqual({ note: 'checked the fixture' });
  });

  test('an emptied box and a read-only field are still never sent', () => {
    const baseline = { finding: 'as first drawn', hold: 'abc' };
    const values = { finding: '  ', hold: 'typed' };
    expect(editedAnswers(values, baseline, new Set(['hold']))).toEqual({});
  });

  test('a box that appeared after the page was drawn is measured against nothing', () => {
    expect(editedAnswers({ late: 'x' }, {}, new Set())).toEqual({ late: 'x' });
  });
});

describe('followRecord', () => {
  test('an untouched box follows the record, and the record becomes its baseline', () => {
    const drawn = { finding: 'as first drawn' };
    expect(followRecord(drawn, { ...drawn }, { finding: 'newer' })).toEqual({
      values: { finding: 'newer' },
      baseline: { finding: 'newer' },
    });
  });

  test('a box the operator is editing is left as they have it', () => {
    expect(followRecord({ note: 'typing…' }, { note: '' }, { note: 'newer' })).toBeNull();
    // ...while an untouched box beside it still follows.
    expect(
      followRecord({ note: 'typing…', finding: 'x' }, { note: '', finding: 'x' }, {
        note: 'newer',
        finding: 'y',
      }),
    ).toEqual({
      values: { note: 'typing…', finding: 'y' },
      baseline: { note: '', finding: 'y' },
    });
  });

  test('nothing changed is null, so the page does not redraw', () => {
    const drawn = { finding: 'x' };
    expect(followRecord(drawn, { ...drawn }, { finding: 'x' })).toBeNull();
  });

  test('a field the record now holds that the page did not draw joins untouched', () => {
    expect(followRecord({}, {}, { late: 'y' })).toEqual({
      values: { late: 'y' },
      baseline: { late: 'y' },
    });
  });
});
