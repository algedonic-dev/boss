// Playwright globalSetup — seeds the brewery reference data
// into the scratch stack before the smoke suite runs.
//
// Smoke specs reference seeded ids (`acc-bigseed-NNNN`,
// `vnd-bigseed-NNN`, `emp-NNNN`) when POSTing through paired
// services. With BOSS_SCRATCH=1 those writes route to the
// +1000 scratch ports → boss_scratch DB, which starts empty
// on a fresh VM. Without this setup, every spec that touches
// a seeded id 400s with "subject not found" or "account does
// not exist".
//
// We invoke the boss-brewery-data-seed binary directly with
// per-service base URLs pointed at the scratch ports
// (people 8500, inventory 8300, messages 8200). The bulletin
// and reservation seeds skip themselves with a warn when
// their solo-service bases (content 7090, calendar 7860) are
// unreachable from the scratch context — those are prod-only
// services; smoke specs that need them set BOSS_SCRATCH=0.
//
// PWTEST_SKIP_SEED=1 disables the seed for fast iteration on
// non-data-dependent specs. PWTEST_SKIP_DEVSERVER=1 implies
// SKIP_SEED so a dev driving an external dev-server doesn't
// get auto-seeded under them.

import { spawn } from 'node:child_process';

const SEEDER_BINARY = '/opt/boss/target/release/boss-brewery-data-seed';

export default async function globalSetup(): Promise<void> {
  if (process.env['PWTEST_SKIP_DEVSERVER'] === '1') {
    return;
  }
  if (process.env['PWTEST_SKIP_SEED'] === '1') {
    return;
  }
  // Only seed when the suite is actually pointed at scratch.
  // BOSS_SCRATCH=0 means smoke is running against prod stack,
  // which already has brewery data via the regular regen path.
  const scratchMode = process.env['BOSS_SCRATCH'] ?? '1';
  if (scratchMode !== '1') {
    return;
  }

  // Step 1: copy reference data (locations + baseline employees)
  // from prod → scratch. boss-locations-api is solo / prod-only
  // by design (locations are shared between scratch + prod;
  // same physical world, same loc-brewery-brewhouse). But the
  // per-DB FK constraints (`employees_location_fkey`,
  // `account_team_members_employee_id_fkey`) mean scratch needs
  // the rows present locally.
  //
  // Use UPSERT (`ON CONFLICT DO NOTHING`) instead of TRUNCATE
  // so rerunning globalSetup doesn't wipe baseline employees
  // (emp-cto, emp-coo, emp-owner, emp-smoke) that the schema
  // migration seeds and other test fixtures depend on. The
  // brewery data-seeder is itself idempotent so the second
  // pass is a no-op.
  console.log('[globalSetup] copying reference data prod → scratch…');
  await new Promise<void>((resolve, reject) => {
    const child = spawn('bash', ['-c',
      // Two passes: locations first (free of dependencies),
      // baseline employees second (which depend on locations).
      // Both pull from prod via COPY (...) TO STDOUT, then
      // upsert into scratch via INSERT … ON CONFLICT DO NOTHING.
      `set -e
      PGPASSWORD=boss psql -U boss -h 127.0.0.1 boss_scratch -c \
        "CREATE TEMP TABLE _staged_locations (LIKE locations INCLUDING ALL); \
         COPY _staged_locations FROM PROGRAM 'PGPASSWORD=boss psql -U boss -h 127.0.0.1 -At -c \\\"COPY (SELECT * FROM locations) TO STDOUT\\\" boss'; \
         INSERT INTO locations SELECT * FROM _staged_locations ON CONFLICT (id) DO NOTHING;"
      PGPASSWORD=boss psql -U boss -h 127.0.0.1 boss_scratch -c \
        "CREATE TEMP TABLE _staged_employees (LIKE employees INCLUDING ALL); \
         COPY _staged_employees FROM PROGRAM 'PGPASSWORD=boss psql -U boss -h 127.0.0.1 -At -c \\\"COPY (SELECT * FROM employees WHERE id IN (''emp-cto'', ''emp-coo'', ''emp-owner'', ''emp-smoke'', ''emp-ceo'', ''emp-cfo'')) TO STDOUT\\\" boss'; \
         INSERT INTO employees SELECT * FROM _staged_employees ON CONFLICT (id) DO NOTHING;"`,
    ], { stdio: 'inherit' });
    child.on('error', reject);
    child.on('exit', (code) => {
      if (code === 0) resolve();
      else reject(new Error(`reference-data copy exited with ${code}`));
    });
  });

  // Step 2: run the brewery data-seed against the scratch ports.
  console.log('[globalSetup] seeding brewery data into scratch stack…');
  await new Promise<void>((resolve, reject) => {
    const child = spawn(
      SEEDER_BINARY,
      [
        '--people-base',    'http://127.0.0.1:8500',
        '--inventory-base', 'http://127.0.0.1:8300',
        '--messages-base',  'http://127.0.0.1:8200',
        // Solo services have no scratch counterpart. Point at
        // a deliberately unreachable port so the binary's
        // reachability probe falls into its skip branch — we
        // do NOT want the seeder to POST bulletins /
        // reservations into prod from a scratch context.
        // 127.0.0.1:1 is reserved + always closed.
        '--content-base',   'http://127.0.0.1:1',
        '--calendar-base',  'http://127.0.0.1:1',
      ],
      {
        env: {
          ...process.env,
          // Forward scratch-mode signal in case future seeder
          // logic gates on it.
          BOSS_SCRATCH: '1',
        },
        stdio: 'inherit',
      },
    );
    child.on('error', reject);
    child.on('exit', (code) => {
      if (code === 0) resolve();
      else reject(new Error(`boss-brewery-data-seed exited with code ${code}`));
    });
  });
  console.log('[globalSetup] scratch seed complete');
}
