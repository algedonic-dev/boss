export type HumanPresentation = Readonly<{
  ask: string;
  why_person: string;
  /** A label for opening the existing step surface, never executable text. */
  action: string;
  after: string;
  evidence: 'current-step';
}>;

export type HumanPresentationView =
  | Readonly<{ kind: 'other' }>
  | Readonly<{ kind: 'missing' }>
  | Readonly<{ kind: 'ready'; presentation: HumanPresentation }>;

export function humanPresentation(metadata?: Record<string, unknown> | null): HumanPresentationView {
  const flag = metadata?.human_only;
  if (!(flag === true || (typeof flag === 'string' && flag.trim().toLowerCase() === 'true'))) return { kind: 'other' };
  const value = metadata?.human_presentation;
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return { kind: 'missing' };
  const row = value as Record<string, unknown>;
  for (const key of ['ask', 'why_person', 'action', 'after']) {
    if (typeof row[key] !== 'string' || !row[key].trim()) return { kind: 'missing' };
  }
  if (row.evidence !== 'current-step') return { kind: 'missing' };
  return { kind: 'ready', presentation: {
    ask: row.ask as string, why_person: row.why_person as string,
    action: row.action as string, after: row.after as string, evidence: 'current-step',
  } };
}
