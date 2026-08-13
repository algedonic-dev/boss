import { describe, expect, test } from 'bun:test';
import { assignmentPacket, splitQueues, type AssignmentRow } from './assignments';

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

describe('assignmentPacket', () => {
  test('maps a row onto the packet-card grammar', () => {
    const p = assignmentPacket(row({}));
    expect(p.id).toBe('j1');
    expect(p.kind).toBe('field-service');
    expect(p.title).toBe('Fix the kettle');
    expect(p.branch).toBe('Do it');
    expect(p.tags).toEqual([]);
    expect(p.sim).toBe(false);
    expect(p.skipReason).toBeNull();
  });

  test('non-standard priority and due date ride as tag chips', () => {
    const p = assignmentPacket(row({ priority: 'urgent', due_on: '2026-08-13' }));
    expect(p.tags).toEqual(['urgent', 'due 2026-08-13']);
  });

  test('a not-yet-actionable step is tagged blocked', () => {
    const p = assignmentPacket(row({ step: { status: 'pending' } }));
    expect(p.tags).toEqual(['blocked']);
    expect(assignmentPacket(row({ step: { status: 'active' } })).tags).toEqual([]);
  });

  test('a simulated packet is marked SIM in the personal queue too', () => {
    expect(assignmentPacket(row({ simulated: true })).sim).toBe(true);
    // Pre-column packets carry the tag instead — the same fallback the
    // yard uses, so one packet cannot read sim in one lens and real in
    // the other.
    expect(assignmentPacket(row({ tags: ['sim'] })).sim).toBe(true);
    expect(assignmentPacket(row({ simulated: false, tags: ['hotfix'] })).sim).toBe(false);
  });
});
