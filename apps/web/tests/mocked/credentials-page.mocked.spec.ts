// /it/registry/credentials — the credentials registry drawn, its
// lifecycle offered as packets (backlog 851259b9; design 76155676 step 3,
// David 2026-09-27).
//
// The crawls cover the empty registry (the mock floor's `[]`) and the
// outage. This spec draws three rows shaped like the system of record's
// on 2026-09-27 — one root, one the broker mints, one placed by hand —
// and pins what a reader must be able to do: tell the three apart, see
// every missing fact drawn as a gap, file a rotation on the credential's
// own id, file a retire and a declare as backlog items, and never see a
// value, even one a body planted. A failed registry read draws nothing
// but the failure; a failed rule read is said on the cards it affects.

import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

const PAGE = '/it/registry/credentials';
const FILED = '5b1e0c4a-2f6d-4c1e-9a7b-3d8e2f1c0a9b';
const PLANTED = 'PLANTED-VALUE-9f8e7d6c';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const ROWS = [
  {
    id: 'boss-credential-broker-root',
    kind: 'forgejo-access-token',
    issuer: 'forgejo (the forge host)',
    principal: 'user david (admin)',
    scopes: [],
    storage_location: 'k8s Secret boss/boss-credential-broker-root key forgejo-token',
    consumers: [{ kind: 'env', location: 'dispatcher env BOSS_BROKER_FORGEJO_TOKEN in the boss pod (boss.yaml)' }],
    rotation_policy: 'on-demand',
    rotated_at: null,
    notes: 'Minted once by David in a passkey-authorized ceremony on 2026-09-02.',
    // A value planted in the body: the page must never draw it.
    value: PLANTED,
  },
  {
    id: 'forge-host-checkout-token',
    kind: 'forgejo-access-token',
    issuer: 'forgejo (the forge host)',
    principal: 'user david',
    scopes: ['write:repository'],
    storage_location: 'k8s Secret boss/forge-host-checkout-token key token',
    consumers: [{ kind: 'file', location: 'the forge host checkout /home/david/boss' }],
    rotation_policy: 'on-demand',
    rotated_at: '2026-09-26T23:05:58.924649Z',
    notes: '',
  },
  {
    id: 'hand-placed-vendor-key',
    kind: 'api-key',
    issuer: 'vendor.example (minted by hand in its web UI)',
    principal: 'account example-operator',
    scopes: [],
    storage_location: '/etc/example/vendor.key on the forge host',
    consumers: [],
    rotation_policy: 'on-demand',
    rotated_at: null,
    notes: '',
  },
];

const RULES = {
  rules: [
    {
      name: 'broker-rotates-the-forge-host-checkout-token',
      on_event: 'step.done.credential-rotation',
      when: 'subject_id = "forge-host-checkout-token"',
      do: [{ handler: 'credential.rotate.forgejo', args: {} }],
      version: 1,
      status: 'active',
    },
  ],
  handler_emits: {},
  system_edges: [],
};

type Install = Readonly<{
  registry?: (r: Route) => Promise<void>;
  rules?: (r: Route) => Promise<void>;
  posts?: Array<Record<string, unknown>>;
}>;

async function install(page: Page, opts: Install = {}): Promise<void> {
  await installSmokeMocks(page);
  // Signed in as the smoke persona, so a filed packet has an owner.
  await page.route(/\/api\/session$/, (r) => json(r, { username: 'ceo@demo', employee_id: 'emp-001', role: 'ceo' }));
  await page.route(/\/api\/credentials$/, opts.registry ?? ((r) => json(r, ROWS)));
  await page.route(/\/api\/dispatcher\/rules$/, opts.rules ?? ((r) => json(r, RULES)));
  await page.route(
    (u) => u.pathname === '/api/jobs',
    (r) => {
      if (r.request().method() !== 'POST') return r.fallback();
      opts.posts?.push(r.request().postDataJSON() as Record<string, unknown>);
      return json(r, { id: FILED }, 201);
    },
  );
}

const card = (page: Page, id: string) => page.locator(`section.cr-card[aria-label="${id}"]`);
const fact = (page: Page, id: string, label: string) => card(page, id).locator(`dt:has-text("${label}") + dd`);

test.describe('/it/registry/credentials', () => {
  test('tells root, broker and hand apart, and draws every missing fact as a gap', async ({ page }) => {
    await install(page);
    await mountPage(page, PAGE, { titleMatch: /Credentials/ });

    await expect(page.locator('section.cr-card')).toHaveCount(3);
    await expect(page.locator('.cr-summary')).toContainText('3 credentials · 1 root · 1 minted by the broker · 1 placed by hand');

    await expect(fact(page, 'boss-credential-broker-root', 'Provenance')).toContainText('Root — placed by David');
    await expect(fact(page, 'forge-host-checkout-token', 'Provenance')).toContainText('Minted by the broker');
    await expect(fact(page, 'forge-host-checkout-token', 'Provenance')).toContainText('credential.rotate.forgejo');
    await expect(fact(page, 'forge-host-checkout-token', 'Provenance')).toContainText('broker-rotates-the-forge-host-checkout-token');
    // Which root it is minted from is not a registry fact: a gap, not a guess.
    await expect(fact(page, 'forge-host-checkout-token', 'Provenance').locator('.cr-gap')).toContainText('Minted from: not recorded');
    await expect(fact(page, 'hand-placed-vendor-key', 'Provenance')).toContainText('Placed by hand');

    // Every registry fact, drawn.
    await expect(fact(page, 'forge-host-checkout-token', 'Scopes')).toHaveText('write:repository');
    await expect(fact(page, 'forge-host-checkout-token', 'Stored at')).toHaveText('k8s Secret boss/forge-host-checkout-token key token');
    await expect(fact(page, 'forge-host-checkout-token', 'Consumers')).toContainText('the forge host checkout /home/david/boss');
    await expect(fact(page, 'forge-host-checkout-token', 'Last rotation')).toHaveText('2026-09-26 23:05Z');
    await expect(fact(page, 'forge-host-checkout-token', 'Next rotation due')).toContainText('on demand');
    await expect(card(page, 'forge-host-checkout-token').locator('.cr-gaps')).toHaveCount(0);

    // The hand-placed key: no consumer, no rotation, unverified scopes — each a gap.
    await expect(fact(page, 'hand-placed-vendor-key', 'Consumers').locator('.cr-gap')).toHaveText('none declared');
    await expect(fact(page, 'hand-placed-vendor-key', 'Last rotation')).toHaveClass(/cr-gap/);
    await expect(fact(page, 'hand-placed-vendor-key', 'Last rotation')).toHaveText('never recorded');
    await expect(fact(page, 'hand-placed-vendor-key', 'Scopes')).toHaveClass(/cr-gap/);
    await expect(card(page, 'hand-placed-vendor-key').locator('.cr-gaps')).toContainText(
      'no consumer declared · no rotation recorded · scopes unverified',
    );

    // The page holds no value, even one the body carried.
    await expect(page.locator('body')).not.toContainText(PLANTED);
    await expect(page.locator('.load-failed')).toHaveCount(0);
  });

  test('rotate files rotate-a-credential on the credential id, and links the packet', async ({ page }) => {
    const posts: Array<Record<string, unknown>> = [];
    await install(page, { posts });
    await mountPage(page, PAGE, { titleMatch: /Credentials/ });

    await card(page, 'forge-host-checkout-token').getByRole('button', { name: 'Rotate' }).click();
    const filed = card(page, 'forge-host-checkout-token').locator('.cr-filed');
    await expect(filed).toContainText('Rotation filed');
    await expect(filed.locator('a')).toHaveAttribute('href', `/ux/jobs/${FILED}`);
    await expect(card(page, 'forge-host-checkout-token').getByRole('button', { name: 'Rotate' })).toBeDisabled();

    expect(posts).toHaveLength(1);
    expect(posts[0]).toMatchObject({
      kind: 'rotate-a-credential',
      title: 'Rotate forge-host-checkout-token',
      subject: { id: 'forge-host-checkout-token', subject_kind: 'custom' },
      owner_id: 'emp-001',
      metadata: { credential: 'forge-host-checkout-token', requested_from: PAGE },
    });
  });

  test('retire and declare file backlog items; a value-shaped id is refused before anything is filed', async ({ page }) => {
    const posts: Array<Record<string, unknown>> = [];
    await install(page, { posts });
    await mountPage(page, PAGE, { titleMatch: /Credentials/ });

    await card(page, 'hand-placed-vendor-key').getByRole('button', { name: 'Retire' }).click();
    await expect(card(page, 'hand-placed-vendor-key').locator('.cr-filed')).toContainText('Retire item filed');

    const form = page.locator('form.cr-declare');
    const submit = form.getByRole('button', { name: 'File a declare item' });
    await form.locator('input').fill('3f9a0c1e5b7d2a4c6e8f0a1b3c5d7e9f1a2b3c4d');
    await expect(form).toContainText('looks like a value, not a name');
    await expect(submit).toBeDisabled();
    // A pasted token that is not even kebab-case is refused as a value,
    // and the refusal never draws what was typed.
    await form.locator('input').fill('ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8');
    await expect(form).toContainText('looks like a value, not a name');
    await expect(page.locator('body')).not.toContainText('ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8');
    await expect(submit).toBeDisabled();
    await form.locator('input').fill('hand-placed-vendor-key');
    await expect(form).toContainText('already declared');
    await expect(submit).toBeDisabled();
    await form.locator('input').fill('github-app-installation');
    await submit.click();
    await expect(form.locator('.cr-filed')).toContainText('Declare item filed');

    expect(posts.map((p) => [p.kind, p.title, (p.subject as { id: string }).id])).toEqual([
      ['backlog-item', 'Retire the credential hand-placed-vendor-key', 'hand-placed-vendor-key'],
      ['backlog-item', 'Declare the credential github-app-installation in the registry', 'github-app-installation'],
    ]);
  });

  test('a refused filing is said on the card, with the status the server gave', async ({ page }) => {
    await install(page);
    await page.route(
      (u) => u.pathname === '/api/jobs',
      (r) => (r.request().method() === 'POST' ? r.fulfill({ status: 403, body: 'forbidden' }) : r.fallback()),
    );
    await mountPage(page, PAGE, { titleMatch: /Credentials/ });

    await card(page, 'forge-host-checkout-token').getByRole('button', { name: 'Rotate' }).click();
    await expect(card(page, 'forge-host-checkout-token').locator('.load-failed')).toContainText(
      'Rotation not filed: file rotate-a-credential: HTTP 403: forbidden',
    );
  });

  test('a failed registry read draws the failure and nothing else — it is not an empty registry', async ({ page }) => {
    await install(page, { registry: (r) => r.fulfill({ status: 403, body: '' }) });
    await mountPage(page, PAGE, { titleMatch: /Credentials/ });

    await expect(page.locator('.cr-fail.load-failed')).toContainText('The credentials registry did not answer: /api/credentials: HTTP 403');
    await expect(page.locator('section.cr-card')).toHaveCount(0);
    await expect(page.locator('.cr-notice')).toHaveCount(0);
    await expect(page.locator('form.cr-declare')).toHaveCount(0);
  });

  test('a failed rule read is said on each card it affects, and the registry still stands', async ({ page }) => {
    await install(page, { rules: (r) => r.fulfill({ status: 503, body: '' }) });
    await mountPage(page, PAGE, { titleMatch: /Credentials/ });

    await expect(page.locator('section.cr-card')).toHaveCount(3);
    // A root is known from where it lives; the others cannot be told.
    await expect(fact(page, 'boss-credential-broker-root', 'Provenance')).toContainText('Root — placed by David');
    await expect(fact(page, 'hand-placed-vendor-key', 'Provenance').locator('.load-failed')).toContainText(
      "could not tell — the broker's rules did not answer",
    );
    await expect(page.locator('.cr-summary')).not.toContainText('placed by hand');
  });
});
