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
  const value = metadata?.human_presentation;
  // human_only REQUIRES a presentation; it is not the only way to have
  // one. A person-decided step kept off passkey presence declares its
  // ask without the flag (design-doc's review, A163052), and gating on
  // the flag alone drew that ask as a bare card in David's queue
  // (bf47083d, measured 2026-11-11 on design-doc 558ca6d7).
  const human = flag === true || (typeof flag === 'string' && flag.trim().toLowerCase() === 'true');
  if (!human && (value === undefined || value === null)) return { kind: 'other' };
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
