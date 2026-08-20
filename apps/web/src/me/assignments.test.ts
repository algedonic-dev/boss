import { describe, expect, test } from 'bun:test';
import {
  assignmentPacket,
  filterByProtocol,
  protocolCounts,
  needsAPerson,
  splitQueues,
  type AssignmentRow,
} from './assignments';

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

// David, 2026-08-16: "a special separation between jobs that are in a
// queue with a human-only policy with jobs that agents are also
// eligible for as a practical consideration."
describe('the human/agent separation', () => {
  test('an agent-completion step is kept out of the claimable queue', () => {
    const q = splitQueues(
      [
        row({ step: { assignee_id: null, completion: 'human' } }),
        row({ step: { assignee_id: null, kind: 'demand-gate', completion: 'agent' } }),
      ],
      'me',
    );
    // The point of the split: a person scanning "up for grabs" must
    // not be offered a step the dispatcher is supposed to execute.
    // Claiming one is how a protocol silently becomes manual.
    expect(q.upForGrabs.length).toBe(1);
    expect(q.upForGrabs[0]?.step.completion).toBe('human');
    expect(q.notMineToDo.length).toBe(1);
    expect(q.notMineToDo[0]?.step.kind).toBe('demand-gate');
  });

  test('nothing is dropped — the two buckets partition the unclaimed', () => {
    const rows = [
      row({ step: { assignee_id: null, completion: 'human' } }),
      row({ step: { assignee_id: null, completion: 'agent' } }),
      row({ step: { assignee_id: null, completion: 'child-job' } }),
      row({ step: { assignee_id: null, completion: 'external' } }),
      row({ step: { assignee_id: null, completion: 'auto-on-materialize' } }),
    ];
    const q = splitQueues(rows, 'me');
    expect(q.upForGrabs.length + q.notMineToDo.length).toBe(rows.length);
    // Only `human` is a person's job; every other contract completes
    // by some mechanism that is not somebody reading a queue.
    expect(q.upForGrabs.length).toBe(1);
  });

  test('an unknown contract reads as human, not as agent-eligible', () => {
    // A tenant protocol can name a step kind this deployment has not
    // registered, and the server sends null. Erring toward "a person
    // should look at this" costs a glance; erring the other way files
    // real work under "an agent will get to it" and it stalls.
    for (const completion of [undefined, null]) {
      expect(needsAPerson(row({ step: { assignee_id: null, completion } }))).toBe(true);
    }
    const q = splitQueues([row({ step: { assignee_id: null } })], 'me');
    expect(q.upForGrabs.length).toBe(1);
    expect(q.notMineToDo.length).toBe(0);
  });

  test('a claimed agent step stays with its claimant, not in the automation list', () => {
    // The split only ever partitions UNCLAIMED rows. Somebody already
    // holding an agent-completion step is mid-flight on it, and moving
    // it out from under them would lose the assignment in the UI.
    const q = splitQueues(
      [row({ step: { assignee_id: 'me', completion: 'agent' } })],
      'me',
    );
    expect(q.mine.length).toBe(1);
    expect(q.notMineToDo.length).toBe(0);
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

describe('protocol filtering', () => {
  const queues = {
    mine: [row({ workflow: 'approval' }), row({ workflow: 'ship-a-change' })],
    upForGrabs: [row({ workflow: 'approval' })],
    notMineToDo: [row({ workflow: 'demand-forecast' })],
    inFlightElsewhere: [row({ workflow: 'user-feedback' })],
    verdicts: [],
  };

  test('counts every protocol across all four queues, busiest first', () => {
    expect(protocolCounts(queues)).toEqual([
      { workflow: 'approval', count: 2 },
      // `demand-forecast` is only in notMineToDo — a chip that skipped
      // that queue would send the reader to an empty-looking filter
      // that then renders a row.
      { workflow: 'demand-forecast', count: 1 },
      { workflow: 'ship-a-change', count: 1 },
      { workflow: 'user-feedback', count: 1 },
    ]);
  });

  test('no queues means no chips, rather than a crash', () => {
    expect(protocolCounts(null)).toEqual([]);
  });

  test('null filter is every row; a protocol keeps only its own', () => {
    expect(filterByProtocol(queues.mine, null)).toHaveLength(2);
    expect(filterByProtocol(queues.mine, 'approval')).toHaveLength(1);
  });

  test('a stale chip selection empties the list instead of widening it', () => {
    // The chip for a protocol that has since drained must not read as
    // "no filter" — that would silently show everything at the moment
    // the operator believes they are looking at one protocol.
    expect(filterByProtocol(queues.mine, 'retired-protocol')).toHaveLength(0);
  });
});

// d598681f, accepted 2026-08-19: verdicts split from owned work.
import { isVerdict } from './assignments';

describe('verdicts split from owned work', () => {
  const row = (kind: string, completion?: 'human' | 'agent') =>
    ({
      job_id: 'j1', job_title: 't', workflow: 'w', subject_kind: 'custom',
      subject_id: 's', priority: 'standard',
      step: { id: kind, job_id: 'j1', kind, title: 't', status: 'ready',
              assignee_id: 'me', completion: completion ?? 'human' },
    }) as never;

  test('sign-offs and reviews are verdicts; tasks are owned work', () => {
    expect(isVerdict(row('sign-off'))).toBe(true);
    expect(isVerdict(row('review-design'))).toBe(true);
    expect(isVerdict(row('correction-verdict'))).toBe(true);
    expect(isVerdict(row('task'))).toBe(false);
    expect(isVerdict(row('checklist'))).toBe(false);
  });

  test('an agent-completed kind is never a verdict for a person', () => {
    expect(isVerdict(row('sign-off', 'agent'))).toBe(false);
  });

  test('the registry flag outranks the client roster both ways', () => {
    // A new decision kind the roster has never heard of: the server
    // says decides, the client believes it.
    const flagged = row('novel-verdict') as { step: { decision_shaped?: boolean } };
    flagged.step.decision_shaped = true;
    expect(isVerdict(flagged as never)).toBe(true);
    // And the server saying "not a decision" beats a roster kind.
    const unflagged = row('sign-off') as { step: { decision_shaped?: boolean } };
    unflagged.step.decision_shaped = false;
    expect(isVerdict(unflagged as never)).toBe(false);
  });

  test('splitQueues partitions assigned rows into verdicts and mine', () => {
    const q = splitQueues([row('sign-off'), row('task')], 'me');
    expect(q.verdicts.map((r) => r.step.kind)).toEqual(['sign-off']);
    expect(q.mine.map((r) => r.step.kind)).toEqual(['task']);
  });
});
