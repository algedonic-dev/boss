import { expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { humanPresentation } from './humanPresentation';

test('every explicitly human platform row has an actionable presentation', () => {
  const dir = new URL('../../../../infra/platform/workflows/', import.meta.url);
  const declared: string[] = [];
  const ready: string[] = [];
  for (const file of readdirSync(dir).filter((f) => f.endsWith('.toml'))) {
    const bundle = Bun.TOML.parse(readFileSync(new URL(file, dir), 'utf8')) as {
      workflow: { kind: string; step: { title: string; metadata_defaults?: Record<string, unknown> }[] }[];
    };
    for (const workflow of bundle.workflow) for (const step of workflow.step) {
      const view = humanPresentation(step.metadata_defaults);
      if (view.kind === 'other') continue;
      declared.push(`${workflow.kind}/${step.title}`);
      expect(view.kind, `${workflow.kind}/${step.title}`).toBe('ready');
      if (view.kind === 'ready') ready.push(`${workflow.kind}/${step.title}`);
    }
  }
  expect(declared.length).toBeGreaterThan(0);
  expect(ready).toEqual(declared);
});
