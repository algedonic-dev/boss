// Shape of a StepType registry row as served by GET /api/jobs/step-types
// (boss-jobs `StepType`). The StepType registry is the alphabet of legal
// transitions; the authoring surface offers it as the palette vocabulary
// and the inspector's type picker. Deserialized once at the fetch site.

export type StepTypeInfo = {
  kind: string;
  label: string;
  category: string;
  /// UX hint slug (e.g. which StepPlugin surface renders this kind).
  ux: string;
  description: string;
};
