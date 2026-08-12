import { describe, expect, test } from 'bun:test';
import { splitQueues, type AssignmentRow } from './assignments';

type RowOverrides = Omit<Partial<AssignmentRow>, 'step'> & {
  step?: Partial<AssignmentRow['step']>;
};

function row(over: RowOverrides): AssignmentRow {
  return {
    job_id: 'j1',
    job_title: 'Fix the kettle',
    due_on: null,
    workflow: 'field-service',
    subject_kind: 'asset',
    subject_id: 'SYS-1',
    priority: 'standard',
    ...over,
    step: {
      id: Math.random().toString(36).slice(2),
      job_id: 'j1',
      kind: 'task',
      title: 'Do it',
      status: 'ready',
      assignee_id: null,
      ...(over.step ?? {}),
    },
  } as AssignmentRow;
}

describe('splitQueues', () => {
  test('partitions mine / up-for-grabs / in-flight-elsewhere', () => {
    const rows = [
      row({ step: { assignee_id: 'me' } }),
      row({ step: { assignee_id: null } }),
      row({ step: { assignee_id: 'them', status: 'active' } }),
    ];
    const q = splitQueues(rows, 'me');
    expect(q.mine.length).toBe(1);
    expect(q.upForGrabs.length).toBe(1);
    expect(q.inFlightElsewhere.length).toBe(1);
  });

  test('urgent sorts above standard within a queue', () => {
    const q = splitQueues(
      [
        row({ priority: 'standard', step: { assignee_id: 'me' } }),
        row({ priority: 'urgent', step: { assignee_id: 'me' } }),
      ],
      'me',
    );
    expect(q.mine[0]?.priority).toBe('urgent');
  });

  test('a due date outranks no due date at equal priority', () => {
    const q = splitQueues(
      [
        row({ step: { assignee_id: 'me' } }),
        row({ due_on: '2026-08-13', step: { assignee_id: 'me' } }),
      ],
      'me',
    );
    expect(q.mine[0]?.due_on).toBe('2026-08-13');
  });
});
