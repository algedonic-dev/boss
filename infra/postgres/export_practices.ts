import { WORLD } from '../../apps/web/src/seed/world';

const esc = (s: string) => s.replace(/'/g, "''");
const lines: string[] = ['BEGIN;'];

for (const c of WORLD.accounts) {
  lines.push(
    `INSERT INTO accounts VALUES ('${c.id}', '${esc(c.name)}', '${esc(c.director)}', '${esc(c.city)}', '${c.state}', '${c.tier}', '${c.customer_since}', '${c.territory_rep_id}');`
  );
}

lines.push('COMMIT;');
process.stdout.write(lines.join('\n') + '\n');
