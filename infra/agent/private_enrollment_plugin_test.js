import { test, expect } from 'bun:test';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';

test('private enrollment never renders authentication metadata or invokes a transport', () => {
  const nodes = [];
  const make = (tag) => {
    const node = { tag, textContent: '', children: [], append(...items) { this.children.push(...items); }, replaceChildren(...items) { this.children = items; } };
    nodes.push(node);
    return node;
  };
  let mount;
  runInNewContext(readFileSync(new URL('../step-plugins/private-enrollment.js', import.meta.url), 'utf8'), {
    window: { __boss_register_step_plugin(kind, fn) { expect(kind).toBe('private-enrollment'); mount = fn; } },
    document: { createElement: make },
    fetch() { throw new Error('private surface must not use generic transport'); },
  });
  const host = make('host');
  mount(host, { step: { metadata: { auth_json: 'synthetic-private-value', broker_receipt_id: '<script>synthetic-private-value</script>' } }, onUpdate() { throw new Error('no metadata completion'); } });
  expect(nodes.some((n) => ['input', 'textarea', 'iframe'].includes(n.tag))).toBe(false);
  expect(nodes.map((n) => n.textContent).join('\n')).not.toContain('synthetic-private-value');
  expect(nodes.some((n) => n.tag === 'button' && n.disabled)).toBe(true);
});
