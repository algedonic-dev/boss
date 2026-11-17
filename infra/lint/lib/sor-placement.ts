// Read scheduling constraints as YAML, so a label in a comment or a second
// OR term cannot stand in for a required data-node placement (eb93f511).
const field = (value: unknown, key: string): unknown =>
  value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Readonly<Record<string, unknown>>)[key]
    : undefined;
const at = (value: unknown, ...keys: readonly string[]): unknown => keys.reduce(field, value);
const array = (value: unknown): value is readonly unknown[] => Array.isArray(value);

function requiresDatabase(spec: unknown): boolean {
  const terms = at(spec, 'affinity', 'nodeAffinity', 'requiredDuringSchedulingIgnoredDuringExecution', 'nodeSelectorTerms');
  return array(terms) && terms.length > 0 && terms.every((term) => {
    const expressions = field(term, 'matchExpressions');
    return array(expressions) && expressions.some((expression) => {
      const values = field(expression, 'values');
      return field(expression, 'key') === 'boss.dev/purpose'
        && field(expression, 'operator') === 'In'
        && array(values) && values.length > 0 && values.every((value) => value === 'database');
    });
  });
}

const file = process.argv[2];
try {
  if (!file) throw new Error('manifest path missing');
  const documents: readonly unknown[] = (await Bun.file(file).text())
    .split(/^---(?:\s+#.*)?\s*$/m)
    .filter((text) => text.trim().length > 0)
    .map((text): unknown => Bun.YAML.parse(text));
  const namespaces = documents.filter((document) => field(document, 'kind') === 'Namespace');
  const namespace = namespaces.length === 1 ? at(namespaces[0], 'metadata', 'name') : undefined;
  const workloads = documents.filter((document) => ['Deployment', 'StatefulSet'].includes(String(field(document, 'kind'))));
  const pinned = workloads.filter((document) => {
    const spec = at(document, 'spec', 'template', 'spec');
    if (at(spec, 'nodeSelector', 'node-role.kubernetes.io/control-plane') === '') return true;
    return typeof namespace === 'string'
      && field(document, 'kind') === 'StatefulSet'
      && at(document, 'metadata', 'name') === 'postgres'
      && at(document, 'metadata', 'namespace') === namespace
      && requiresDatabase(spec);
  });
  if (pinned.length !== workloads.length) {
    console.error(`sor-control-plane: ${file} has ${workloads.length} workload(s) but only ${pinned.length} required placements`);
    console.error('  Every other workload needs nodeSelector node-role.kubernetes.io/control-plane: "".');
    console.error('  Only the source PostgreSQL may instead require boss.dev/purpose=database in every affinity term.');
    process.exit(1);
  }
  console.log(workloads.length);
} catch (error) {
  console.error(`sor-control-plane: cannot read ${file}: ${String(error)}`);
  process.exit(1);
}
