import { z } from 'zod';
import type { ListedJob, ListedStep } from './types';

export type LifecycleListedStep = Pick<ListedStep, 'id' | 'title' | 'status' | 'sort_order' | 'assignee_id'>;
export type LifecycleListedJob = Pick<ListedJob,
  'id' | 'kind' | 'title' | 'subject' | 'status' | 'priority' | 'opened_on' | 'opened_at' | 'closed_on'
> & { steps: readonly LifecycleListedStep[]; metadata?: unknown };

// Parse the fields this list consumes, whether the server serves SLIM
// or full. No invented metadata/fields to satisfy the detail Step type.
const step = z.object({
  id: z.string().min(1), title: z.string(),
  status: z.enum(['pending', 'ready', 'active', 'completed', 'skipped']),
  sort_order: z.number().int().min(-2_147_483_648).max(2_147_483_647), assignee_id: z.string().nullable(),
});
const packet = z.object({
  id: z.string().min(1), kind: z.string(), title: z.string(),
  subject: z.object({ subject_kind: z.string(), id: z.string() }),
  status: z.enum(['draft', 'open', 'closed', 'cancelled']),
  priority: z.enum(['emergency', 'urgent', 'standard', 'scheduled']),
  opened_on: z.iso.date(), opened_at: z.iso.datetime({ offset: true }).nullable().optional(),
  closed_on: z.iso.date().nullable().default(null),
  steps: z.array(step).refine(steps => new Set(steps.map(step => step.id)).size === steps.length, 'Duplicate listed step identity'),
  // Missing metadata is kept unknown by the department column rather
  // than defaulted to {}, which would invent an absent override.
  metadata: z.json().optional(),
});
const envelope = z.object({ data: z.array(packet), total: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER) })
  .refine(value => value.total >= value.data.length, 'Listed row count exceeds total')
  .refine(value => new Set(value.data.map(packet => packet.id)).size === value.data.length, 'Duplicate listed packet identity');

export function parseListedEnvelope(value: unknown): Readonly<{ data: LifecycleListedJob[]; total: number }> {
  return envelope.parse(value);
}
