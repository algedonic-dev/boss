import { expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { humanPresentation } from './humanPresentation';

test('every explicitly human platform row has an actionable presentation', () => {
  const dir = new URL('../../../../infra/platform/workflows/', import.meta.url);
  let count = 0;
  for (const file of readdirSync(dir).filter((f) => f.endsWith('.toml'))) {
    const bundle = Bun.TOML.parse(readFileSync(new URL(file, dir), 'utf8')) as {
      workflow: { kind: string; step: { title: string; metadata_defaults?: Record<string, unknown> }[] }[];
    };
    for (const workflow of bundle.workflow) for (const step of workflow.step) {
      const view = humanPresentation(step.metadata_defaults);
      if (view.kind === 'other') continue;
      count++;
      expect(view.kind, `${workflow.kind}/${step.title}`).toBe('ready');
    }
  }
  expect(count).toBe(19);
});
