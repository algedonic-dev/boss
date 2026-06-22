// The pure seed for a new kind. The I/O wrappers in designJob.ts are
// verified against the running stack, not here. Run via `bun test`.

import { describe, expect, test } from 'bun:test';
import { initialSpec, readSpec } from './designJob';
import type { Step } from '../jobs/types';

describe('initialSpec', () => {
  test('is the minimal viable kind: one trigger that is also terminal', () => {
    const spec = initialSpec('seasonal-release', 'Seasonal Release', 'production', ['asset']);
    expect(spec.kind).toBe('seasonal-release');
    expect(spec.subject_kinds).toEqual(['asset']);
    expect(spec.steps).toHaveLength(1);
    const s = spec.steps[0]!;
    expect(s.ready_when).toBe('true'); // trigger
    expect(s.terminal).toEqual({ outcome: 'completed' }); // terminal
  });

  test('carries the label/category/description through', () => {
    const spec = initialSpec('x', 'X Label', 'sales', ['account'], 'why x');
    expect(spec.label).toBe('X Label');
    expect(spec.category).toBe('sales');
    expect(spec.description).toBe('why x');
    expect(spec.status).toBe('draft');
  });

  test('description defaults to null', () => {
    expect(initialSpec('x', 'X', 'c', []).description).toBeNull();
  });
});

describe('readSpec', () => {
  function step(metadata: Record<string, unknown>): Step {
    return {
      id: 'step-1',
      job_id: 'job-1',
      kind: 'job-kind-publish',
      title: 'Publish',
      assignee_id: null,
      status: 'pending',
      sort_order: 3,
      blocked_by: [],
      completed_on: null,
      metadata,
    };
  }

  test('returns the spec from job_kind_spec metadata', () => {
    const spec = initialSpec('x', 'X', 'c', ['asset']);
    expect(readSpec(step({ job_kind_spec: spec }))?.kind).toBe('x');
  });

  test('returns null when not yet seeded', () => {
    expect(readSpec(step({}))).toBeNull();
    expect(readSpec(undefined)).toBeNull();
  });
});
