import { expect, test } from 'bun:test';
import { parseListedEnvelope } from './listedEnvelope';

const packet = {
  id: 'packet', kind: 'anything', title: 'Packet', subject: { subject_kind: 'new-kind', id: 'subject' },
  status: 'open', priority: 'standard', opened_on: '2026-09-01', closed_on: null,
  steps: [{ id: 'step', title: 'Inspect', status: 'ready', sort_order: 0, assignee_id: null, slim: true }],
};
const envelope = { data: [packet], total: 1 };

test('consumed listed fields accept slim/full and legacy optional stamps without asking for metadata', () => {
  expect(parseListedEnvelope(envelope).data[0]?.steps?.[0]?.title).toBe('Inspect');
  expect(parseListedEnvelope({ data: [{ ...packet, steps: [] }], total: 1 }).data[0]?.steps).toEqual([]);
  expect(parseListedEnvelope({ data: [], total: 0 })).toEqual({ data: [], total: 0 });
  expect(parseListedEnvelope({ data: [{ ...packet, closed_on: undefined }], total: 1 }).data[0]?.closed_on).toBeNull();
  expect(parseListedEnvelope({ data: [{ ...packet, steps: [{ ...packet.steps[0], metadata: {}, fields: [] }] }], total: 1 }).data).toHaveLength(1);
});

test('native packet metadata stays distinct from an omitted read for department ownership', () => {
  expect(parseListedEnvelope(envelope).data[0]?.metadata).toBeUndefined();
  for (const metadata of [{ department: 'it' }, {}, null, []]) {
    expect(parseListedEnvelope({ data: [{ ...packet, metadata }], total: 1 }).data[0]?.metadata).toEqual(metadata);
  }
});

test('omitted native steps cannot claim that a packet has no current step', () => {
  const { steps: _steps, ...withoutSteps } = packet;
  expect(() => parseListedEnvelope({ data: [withoutSteps], total: 1 })).toThrow();
});

test('omitted native holder cannot claim that a ready step is unassigned', () => {
  const holderStep = packet.steps[0];
  if (!holderStep) throw new Error('Required test fixture step is absent');
  const { assignee_id: _holder, ...withoutHolder } = holderStep;
  expect(() => parseListedEnvelope({ data: [{ ...packet, steps: [withoutHolder] }], total: 1 })).toThrow();
  expect(parseListedEnvelope(envelope).data[0]?.steps?.[0]?.assignee_id).toBeNull();
});

test('malformed consumed fields cannot become empty steps, valid ages or a false count', () => {
  for (const value of [{}, [], { data: [], total: -1 }, { data: [], total: 0.5 }, { data: [packet], total: 0 }, { data: [packet, packet], total: 2 },
    ...[{ steps: {} }, { steps: null }, { opened_at: 'not-a-stamp' }, { opened_on: '2026-02-30' },
      { closed_on: 'bad' }, { status: 'blocked' }, { subject: { subject_kind: 'new', id: 2 } },
      { steps: [{ ...packet.steps[0], assignee_id: 7 }] }, { steps: [{ ...packet.steps[0], status: 'done' }] },
      { steps: [{ ...packet.steps[0], sort_order: 0.5 }] }, { steps: [{ ...packet.steps[0], title: null }] },
      { steps: [packet.steps[0], packet.steps[0]] },
    ].map(delta => ({ data: [{ ...packet, ...delta }], total: 1 }))]) {
    expect(() => parseListedEnvelope(value)).toThrow();
  }
});
