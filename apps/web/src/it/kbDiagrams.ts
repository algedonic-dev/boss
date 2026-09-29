// The four architecture diagrams /it/kb draws, as their Mermaid SOURCE.
//
// One definition (backlog 4718d918; decided on page audit 8cd38edd,
// gap 1, 2026-09-25). The page used to import SVG renders from
// it/kb-assets/, which were copies of docs/architecture/*.svg, which
// were renders of docs/architecture/*.mmd — three files per diagram and
// nothing comparing them. The pod and the gate had no renderer, so every
// edit to a source skipped the regen, and after 467175e7 car B retired
// boss-observability every picture still drew it. The page now imports
// the source text and renders it in the browser (KbDiagram.svelte), so
// a picture cannot disagree with its source: there is no picture to
// commit. kbDiagrams.test.ts holds this list to the directory.
//
// The import attribute makes the bundler hand over the text; the path
// reaches outside apps/web, so the image's spa-build stage copies
// docs/architecture (infra/oss-quickstart/Dockerfile).

import stateSurfacesWork from '../../../../docs/architecture/00-state-surfaces-work.mmd' with { type: 'text' };
import primitives from '../../../../docs/architecture/01-primitives.mmd' with { type: 'text' };
import serviceMap from '../../../../docs/architecture/02-service-map.mmd' with { type: 'text' };
import deployment from '../../../../docs/architecture/03-deployment.mmd' with { type: 'text' };

export type KbDiagram = Readonly<{
  /** The file under docs/architecture/ — the one a reader edits. */
  file: string;
  /** What the picture shows, for assistive tech and the error state. */
  label: string;
  source: string;
}>;

export const KB_DIAGRAMS: ReadonlyArray<KbDiagram> = [
  { file: '00-state-surfaces-work.mmd', label: 'State / Surfaces / Work framing', source: stateSurfacesWork },
  { file: '01-primitives.mmd', label: 'Primitives and abstractions', source: primitives },
  { file: '02-service-map.mmd', label: 'Service map', source: serviceMap },
  { file: '03-deployment.mmd', label: 'Deployment topology', source: deployment },
];

/** The diagram drawn from `file`; throws on a name not in the list,
 *  because every caller is a literal on the page. */
export function kbDiagram(file: string): KbDiagram {
  const d = KB_DIAGRAMS.find((x) => x.file === file);
  if (!d) throw new Error(`kbDiagram: no diagram ${file} under docs/architecture`);
  return d;
}
