// The edge-condition contract: a downstream step whose `ready_when`
// gates on an upstream step's metadata value IS a labeled routing
// edge — `steps.triage.metadata.disposition = "build"` makes the
// triage→build edge carry `build`, which is what lets TriageFlow
// render fork choices as clickable edges instead of a detached
// button row. Predicates without a metadata comparison stay
// unlabeled dependency edges, exactly as before.
import { describe, expect, it } from 'bun:test';
import { workflowToDag } from './workflowToDag';

const STEPS = [
  { title: 'submitted', kind: 'trigger', ready_when: 'true' },
  { title: 'triage', kind: 'task', ready_when: 'steps.submitted.done' },
  {
    title: 'build',
    kind: 'task',
    ready_when: 'steps.triage.done AND steps.triage.metadata.disposition = "build"',
  },
  {
    title: 'declined',
    kind: 'outcome',
    ready_when: 'steps.triage.done AND steps.triage.metadata.disposition = "decline"',
    terminal: { outcome: 'aborted' },
  },
  {
    title: 'closed',
    kind: 'outcome',
    ready_when: 'steps.build.done',
    terminal: { outcome: 'completed' },
  },
] as const;

describe('workflowToDag edge conditions', () => {
  const { edges } = workflowToDag(STEPS as never);
  const edge = (from: string, to: string) => edges.find((e) => e.from === from && e.to === to);

  it('labels a routing edge with the metadata value that opens it', () => {
    expect(edge('triage', 'build')?.label).toBe('build');
    expect(edge('triage', 'declined')?.label).toBe('decline');
  });

  it('carries the parsed condition so a click can complete the fork', () => {
    expect(edge('triage', 'build')?.condition).toEqual({
      field: 'disposition',
      value: 'build',
    });
  });

  it('leaves plain dependency edges unlabeled and unconditioned', () => {
    const plain = edge('submitted', 'triage');
    expect(plain).toBeDefined();
    expect(plain?.label).toBeUndefined();
    expect(plain?.condition).toBeUndefined();
    const done = edge('build', 'closed');
    expect(done?.condition).toBeUndefined();
  });
});
