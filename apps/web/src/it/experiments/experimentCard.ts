// What an experiment's card says — the readers /it/design/experiments
// renders, pure so experimentCard.test.ts can pin them (page-audit
// ec8351f4). Each reads one protocol-experiment step field by its
// step's `spec_slug`; the page only lays them out.
import type { Job, Step } from '../../jobs/types';

const stepsOf = (j: Job): ReadonlyArray<Step> =>
  [...(j.steps ?? [])].sort((a, b) => a.sort_order - b.sort_order);

/// One step field as text: '' when the step or the key is absent.
export const stepField = (j: Job, slug: string, key: string): string => {
  const meta = (stepsOf(j).find((s) => s.spec_slug === slug)?.metadata ?? {}) as Record<string, unknown>;
  const v = meta[key];
  return typeof v === 'string' ? v : v == null ? '' : String(v);
};

/// The terminal the packet closed on (`promoted`, `retired`,
/// `inconclusive`, `abandoned`), or '' while none has.
export const outcomeOf = (j: Job): string => {
  const o = (j.metadata as Record<string, unknown> | undefined)?.outcome;
  return typeof o === 'string' ? o : '';
};

/// The protocol's body (v5 as authored) names the arms `control` /
/// `candidate`, free text. The `*_version` names are the v1–v4 shape
/// of those fields, which only a packet stated under that older body
/// carries — so they are the fallback, not the field.
export const armOf = (j: Job, which: 'control' | 'candidate'): string =>
  stepField(j, 'state', which) || stepField(j, 'state', `${which}_version`);

export type Measured = Readonly<{
  control: string;
  candidate: string;
  samples: string;
  /// `state.sample_floor` — how little data the author said, BEFORE the
  /// result, they would decide on. Beside n it shows whether n cleared it.
  floor: string;
  /// `measure.source` — where the numbers came from.
  source: string;
}>;

/// A concluded card is one of two records. An abandoned packet closes
/// after `state` with no `measure` or `decide` (its terminal's
/// `ready_when` is `steps.state.done AND job.metadata.abandoned`), so
/// its honest content is the `reason` that terminal requires — a row of
/// dashes in the measured and decided slots reads as data lost, not as
/// work stopped on purpose (backlog baf0c973).
export type ConcludedCard =
  | Readonly<{ kind: 'abandoned'; predicted: string; reason: string }>
  | Readonly<{
      kind: 'decided';
      predicted: string;
      measured: Measured;
      decision: string;
      /// `decide.against_stated_rule` — whether the decision followed
      /// the rule written before the result: the falsifiability the
      /// protocol exists for (backlog 3633a918).
      againstRule: string;
      confounds: string;
    }>;

export function concludedCard(j: Job): ConcludedCard {
  const predicted = stepField(j, 'state', 'hypothesis');
  if (outcomeOf(j) === 'abandoned') {
    return { kind: 'abandoned', predicted, reason: stepField(j, 'abandoned', 'reason') };
  }
  return {
    kind: 'decided',
    predicted,
    measured: {
      control: stepField(j, 'measure', 'control_result'),
      candidate: stepField(j, 'measure', 'candidate_result'),
      samples: stepField(j, 'measure', 'samples'),
      floor: stepField(j, 'state', 'sample_floor'),
      source: stepField(j, 'measure', 'source'),
    },
    decision: stepField(j, 'decide', 'decision'),
    againstRule: stepField(j, 'decide', 'against_stated_rule'),
    confounds: stepField(j, 'decide', 'confounds'),
  };
}

/// The Measured line: the two results, then n with the floor stated in
/// advance beside it, then where the numbers came from. `run.started_at`,
/// `run.method` and `decide.elapsed` stay on the packet, one click away
/// through the title: they describe the run's logistics, not whether its
/// conclusion holds (the 3633a918 decision).
export const measuredLine = (m: Measured): string =>
  `control ${m.control || '—'} · candidate ${m.candidate || '—'} · ` +
  `n=${m.samples || '?'} (floor ${m.floor || '—'}) · source: ${m.source || '—'}`;
