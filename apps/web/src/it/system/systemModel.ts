// System Model hub (/system) — the model-definition landing surface. A live-stats launchpad
// into the surfaces that describe the running state machine: Subjects,
// Jobs, Steps, Events, and the registries layered over them. Read-only;
// the cards link out to the existing surfaces.

/** Which live stat a card shows. The page fetches each source once on
 *  mount and fills the card in as it resolves; null cards are pure links. */
export type StatKind =
  | 'jobKinds'
  | 'openJobs'
  | 'rules'
  | 'subjectsClasses'
  | 'stepPlugins'
  | 'lastEvent'
  | null;

export type SurfaceCard = Readonly<{
  id: string;
  title: string;
  blurb: string;
  path: string;
  stat: StatKind;
}>;

/** The hub's surface catalog, ordered to read like the model itself:
 *  the program (Workflows) → work in flight (Jobs) → the reactive layer
 *  (Dispatcher) → the vocabulary (Subjects & Classes) → step UX → the
 *  event log, then the supporting read surfaces. */
export const SURFACE_CARDS: ReadonlyArray<SurfaceCard> = [
  { id: 'workflows', title: 'Workflows', blurb: 'JobKinds — the programs the machine runs.', path: '/system/workflows', stat: 'jobKinds' },
  { id: 'jobs', title: 'Live jobs', blurb: 'Work in flight across every kind right now.', path: '/ux/jobs', stat: 'openJobs' },
  { id: 'dispatcher', title: 'Dispatcher', blurb: 'Reactive rules + the step-side-effect cascade.', path: '/system/dispatcher', stat: 'rules' },
  { id: 'subjects', title: 'Subjects & Classes', blurb: "The model's vocabulary — kinds + reference data.", path: '/system/subjects', stat: 'subjectsClasses' },
  { id: 'step-plugins', title: 'Step plugins', blurb: 'Custom step-UX bundles served to the SPA.', path: '/system/step-plugins', stat: 'stepPlugins' },
  { id: 'audit', title: 'Audit Log', blurb: 'The immutable event log — system of record.', path: '/system/monitoring/events', stat: 'lastEvent' },
  { id: 'monitoring', title: 'Monitoring', blurb: 'Service health, latency, live operational counts.', path: '/system/monitoring', stat: null },
  { id: 'atlas', title: 'Atlas', blurb: 'A map of every published JobKind, by category.', path: '/system/monitoring/atlas', stat: null },
  { id: 'kb', title: 'Knowledge Base', blurb: 'ADRs, architecture diagrams, platform reference.', path: '/system/kb', stat: null },
  { id: 'design', title: 'Design review', blurb: 'Open design questions across docs/design.', path: '/system/design', stat: null },
];

/** Compact count formatting for the stat badges:
 *  27→"27", 1000→"1k", 1200→"1.2k", 12000→"12k", 2_500_000→"2.5M". */
export function fmtCount(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) {
    const k = n / 1000;
    return (k < 10 ? k.toFixed(1).replace(/\.0$/, '') : Math.round(k).toString()) + 'k';
  }
  const m = n / 1_000_000;
  return (m < 10 ? m.toFixed(1).replace(/\.0$/, '') : Math.round(m).toString()) + 'M';
}
