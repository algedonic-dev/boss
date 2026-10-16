import { expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import ts from 'typescript';

// a4896fd2: pin only the two measured consumers. Other specs may need
// real time; raw clock.install is not a tree-wide error by itself.
const consumers = ['yard-refresh.mocked.spec.ts', 'it-map-live.mocked.spec.ts'];
function problems(source: string): string[] {
  const file = ts.createSourceFile('fixture.ts', source, ts.ScriptTarget.Latest, true);
  const found: string[] = [];
  let shared = false;
  function visit(node: ts.Node): void {
    if (ts.isImportDeclaration(node) && ts.isStringLiteral(node.moduleSpecifier)
      && node.moduleSpecifier.text === './_clock') {
      const bindings = node.importClause?.namedBindings;
      shared = !!bindings && ts.isNamedImports(bindings)
        && bindings.elements.some(item => (item.propertyName ?? item.name).text === 'holdClock');
    }
    if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)) {
      const method = node.expression;
      if (['install', 'pauseAt'].includes(method.name.text)
        && ts.isPropertyAccessExpression(method.expression)
        && method.expression.name.text === 'clock') found.push('local clock setup');
    }
    ts.forEachChild(node, visit);
  }
  visit(file);
  if (!shared) found.push('missing shared holdClock import');
  return found;
}

test('the measured consumers use the shared held-clock boundary', () => {
  expect(consumers.length).toBe(2);
  for (const name of consumers) {
    expect(problems(readFileSync(join(import.meta.dir, '../tests/mocked', name), 'utf8')), name).toEqual([]);
  }
});

test('actual local setup is refused even beside a convincing comment or shared import', () => {
  const shared = "import { holdClock } from './_clock';";
  expect(problems(`${shared}\nawait holdClock(page, now);`)).toEqual([]);
  expect(problems(`${shared}\nawait page.clock.install({ time: now });`)).toEqual(['local clock setup']);
  expect(problems("// import { holdClock } from './_clock';\nawait page.clock.install();"))
    .toEqual(['local clock setup', 'missing shared holdClock import']);
  expect(problems(`${shared}\n// page.clock.install() is prose`)).toEqual([]);
});
