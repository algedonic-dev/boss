// The two Views read boundaries (5fd8a2e2). An unchecked successful
// body used to fail later in nameSharers or shownLine, beyond the read's
// catch. Keep the native contract here, before any state is published.
import { z } from '../data/parseResponse';
import type { View, ViewResults } from './types';

const SourceSchema = z.enum(['subjects', 'jobs', 'steps', 'events']);
const LayoutSchema = z.enum(['table', 'list', 'count']);
const CountSchema = z.number().int().nonnegative().safe();

const ViewSchema: z.ZodType<View> = z.object({
  id: z.string(),
  owner_id: z.string(),
  title: z.string(),
  source: SourceSchema,
  // These two defaults are declared on the Rust wire type too.
  filter: z.string().default(''),
  columns: z.array(z.string()).default([]),
  layout: LayoutSchema,
  visibility: z.enum(['private', 'shared']),
  created_at: z.iso.datetime({ offset: true }),
  updated_at: z.iso.datetime({ offset: true }),
});

const ResultsSchema: z.ZodType<ViewResults> = z.object({
  view_id: z.string(),
  source: SourceSchema,
  layout: LayoutSchema,
  // Projection chooses the fields; nested payload/metadata JSON is data.
  rows: z.array(z.record(z.string(), z.unknown())),
  matched: CountSchema,
  pushed_down: CountSchema,
  truncated: z.boolean(),
  scope: z.enum(['all', 'owners']),
}).refine((r) => r.rows.length <= r.matched, {
  message: 'shown rows exceed matched count',
});

export function parseViews(raw: unknown): ReadonlyArray<View> {
  const parsed = z.array(ViewSchema).safeParse(raw);
  if (!parsed.success) throw new Error(`views list body: ${parsed.error.message}`);
  return parsed.data;
}

export function parseViewResults(raw: unknown, view: Pick<View, 'id' | 'source' | 'layout'>): ViewResults {
  const parsed = ResultsSchema.safeParse(raw);
  if (!parsed.success) throw new Error(`views result body: ${parsed.error.message}`);
  const result = parsed.data;
  if (result.view_id !== view.id || result.source !== view.source || result.layout !== view.layout) {
    throw new Error('views result body: identity, source or layout differs from the requested View');
  }
  return result;
}
