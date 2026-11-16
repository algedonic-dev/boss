// An auth failure on any page sends the browser to sign in AND BACK to
// that page — never an error whose only way out is the root page
// (backlog 7d70cad6, David 2026-11-16).
//
// The mode David meets is Cloudflare Access expiring at the edge
// (david_detail_20261116 on the packet). Access answers a lapsed read
// with a 302 to its own login origin; fetch follows it cross-origin,
// that page carries no CORS header, and the browser reports
// `TypeError: Failed to fetch` — no status, so the old 401 interceptor
// never saw it and the page rendered the throw as its error. Here the
// edge is played exactly that way: every /api read answers 302 to an
// Access login on another origin. The page must reload ITSELF — the
// same url, so Access can sign him in and return him through its
// `redirect_url` — and must never be sent to / or /login.
//
// The gateway-session half (a 401 on a data read goes to /login with
// the page as ?next=) is pinned in readonly-session.mocked.spec.ts;
// the decisions themselves, including the loop guard, are unit tests
// in libs/web-kit/src/session/deadSession.test.ts.

import { expect, test, type Page, type Route } from './_test';
import { servePeopleRows } from './_smokeMocks';
import { readsSettled, recordPageRequests } from './_helpers';
import { EDGE_REAUTH_KEY } from '@boss/web-kit/session/deadSession';

const JOB_ID = 'job-access-1';
const PAGE = `/ux/jobs/${JOB_ID}?tab=steps`;
const ACCESS_LOGIN =
  'https://algedonic-team.cloudflareaccess.com/cdn-cgi/access/login/boss.example' +
  `?kid=k&redirect_url=${encodeURIComponent(PAGE)}`;

const STEP = {
  id: 's1', job_id: JOB_ID, kind: 'task', title: 'triage', status: 'active',
  assignee_id: null, sort_order: 0, blocked_by: [], sign_offs_required: [],
  sign_offs: [], completed_on: null, metadata: {}, notes: null, spec_slug: 'triage',
};

const JOB = {
  id: JOB_ID, kind: 'user-feedback', title: 'The page David was reading',
  status: 'open', opened_on: '2026-11-16', due_on: null, closed_on: null,
  owner_id: 'emp-001', priority: 'standard', simulated: false, tags: [],
  subject: { subject_kind: 'custom', id: '/shop' }, metadata: {}, steps: [STEP],
};

const OPERATOR = {
  id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [],
};

const json = (r: Route, b: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

/** A healthy backend for the job page, signed in as the operator. */
async function healthyBackend(page: Page): Promise<void> {
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, JOB));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, [
    { kind: 'task', label: 'Task', category: 'generic', ux: 'inline', description: '' },
  ]));
  await page.route(/\/api\/people$/, (r) => json(r, [OPERATOR]));
  await servePeopleRows(page, [OPERATOR]);
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: OPERATOR.id, role: OPERATOR.role }));
  await page.route(/\/api\/auth\/me$/, (r) => json(r, 'unauthenticated', 401));
}

/** Every document navigation the main frame makes, in order. */
function documentLoads(page: Page): string[] {
  const loads: string[] = [];
  page.on('request', (req) => {
    if (req.isNavigationRequest() && req.frame() === page.mainFrame()) {
      const u = new URL(req.url());
      loads.push(u.pathname + u.search);
    }
  });
  return loads;
}

/** Number each document this tab loads — 1, 2, … — counted in
 *  sessionStorage, which a reload keeps. A navigation REQUEST
 *  (documentLoads) is sent while the old document is still the one a
 *  read reaches, and Playwright's framenavigated also fires on the
 *  router's own pushState, so neither says which document is current;
 *  the document says so itself. Each document also keeps the loop
 *  guard's reload stamp as it found it, so a reload it issued itself
 *  shows as a stamp that moved. Install before the first goto. */
async function numberDocuments(page: Page): Promise<void> {
  await page.addInitScript((stampKey) => {
    const n = Number(sessionStorage.getItem('mocked.document-number') ?? '0') + 1;
    sessionStorage.setItem('mocked.document-number', String(n));
    const w = window as unknown as { __documentNumber?: number; __stampAtStart?: string | null };
    w.__documentNumber = n;
    w.__stampAtStart = sessionStorage.getItem(stampKey);
  }, EDGE_REAUTH_KEY);
}

type DocumentState = Readonly<{
  n: number;
  /** The fetch guard's own edge probe has been answered here. */
  probed: boolean;
  /** The loop guard's stamp when this document started, and now. */
  stampAtStart: string | null;
  stampNow: string | null;
}>;

/** The current document's state; null while no document can be read.
 *
 *  The probe is the `/api/session` entry in the page's request record
 *  (recordPageRequests) that is SETTLED and never READ: the guard asks
 *  it with redirect manual and reads only the answer's type, while the
 *  app's own `/api/session`, followed into the Access redirect, rejects
 *  and is marked read at rejection. The guard decides — reload or stay
 *  — in the microtasks that answer delivers, and a reload writes the
 *  stamp before it navigates, so a probe seen settled from this (later)
 *  task is a decision already made, and the stamp says which. */
async function currentDocument(page: Page): Promise<DocumentState | null> {
  return page
    .evaluate((stampKey) => {
      const w = window as unknown as {
        __documentNumber?: number;
        __stampAtStart?: string | null;
        __pageRequests?: Array<{ path: string; settled: boolean; read: boolean }>;
      };
      return {
        n: w.__documentNumber ?? 0,
        probed: (w.__pageRequests ?? []).some((e) => e.path === '/api/session' && e.settled && !e.read),
        stampAtStart: w.__stampAtStart ?? null,
        stampNow: sessionStorage.getItem(stampKey),
      };
    }, EDGE_REAUTH_KEY)
    .catch(() => null);
}

test.describe('Access expiry at the edge', () => {
  test('a read the edge redirects reloads the SAME page, which Access returns to', async ({ page }) => {
    await healthyBackend(page);
    // The Access login page: another origin, no CORS header — what
    // turns a followed redirect into `TypeError: Failed to fetch`.
    await page.route('https://algedonic-team.cloudflareaccess.com/**', (r) =>
      r.fulfill({ status: 200, contentType: 'text/html', body: '<p>Cloudflare Access</p>' }));

    // Access is lapsed until the browser loads a DOCUMENT again — that
    // navigation is where Access signs him in and sends him back.
    let accessLapsed = true;
    const loads = documentLoads(page);
    page.on('request', (req) => {
      if (req.isNavigationRequest() && req.frame() === page.mainFrame() && loads.length >= 2) {
        accessLapsed = false;
      }
    });
    await page.route('**/api/**', (r) =>
      accessLapsed
        ? r.fulfill({ status: 302, headers: { location: ACCESS_LOGIN } })
        : r.fallback());

    await page.goto(PAGE);

    // He lands back on the page he was reading, rendered.
    await expect(page.getByText('The page David was reading').first()).toBeVisible();
    const here = new URL(page.url());
    expect(here.pathname + here.search).toBe(PAGE);
    // Exactly one re-authentication: the first load and the reload of
    // the same url. Never / and never /login.
    expect(loads).toEqual([PAGE, PAGE]);
  });

  test('the steady state: a healthy edge never reloads the page', async ({ page }) => {
    await healthyBackend(page);
    await recordPageRequests(page);
    const loads = documentLoads(page);
    await page.goto(PAGE);
    await expect(page.getByText('The page David was reading').first()).toBeVisible();
    // Every read the page opened has had its answer: nothing is left
    // that could send it anywhere.
    await readsSettled(page);
    expect(loads).toEqual([PAGE]);
  });

  test('an edge that keeps redirecting does not reload forever', async ({ page }) => {
    await healthyBackend(page);
    await page.route('https://algedonic-team.cloudflareaccess.com/**', (r) =>
      r.fulfill({ status: 200, contentType: 'text/html', body: '<p>Cloudflare Access</p>' }));
    await page.route('**/api/**', (r) =>
      r.fulfill({ status: 302, headers: { location: ACCESS_LOGIN } }));
    await numberDocuments(page);
    await recordPageRequests(page);
    const loads = documentLoads(page);

    await page.goto(PAGE);
    // The reload happens once; the reloaded page asks again inside the
    // quiet window and is refused, so the tab settles on the page. The
    // wait is for that refusal itself: the guard's edge probe answered
    // on the SECOND document, which is the moment reauthenticateAtEdge
    // chose (currentDocument says why that is already decided), and the
    // stamp the first document's reload wrote still standing unmoved is
    // the choice to stay. Not the job page's error line — under this
    // edge the reloaded document never drew it inside the 15 s budget
    // (run 1c2d1e5a) — and not "every read settled": the keepalive
    // POST /api/surface-opens never settles under a redirecting edge.
    // It replaced a fixed 1 500 ms sleep the suite's sleep floor
    // refuses (backlog 7d70cad6).
    let decided: DocumentState | null = null;
    await expect
      .poll(async () => {
        const d = await currentDocument(page);
        if (d !== null && d.n === 2 && d.probed) decided = d;
        return decided !== null;
      }, { intervals: [25], message: "waiting for the reloaded document's edge probe to be answered" })
      .toBe(true);
    const second = decided as DocumentState | null;
    expect(second?.stampAtStart, 'the first document reloaded through the guard').not.toBeNull();
    expect(second?.stampNow, 'the second document issued no reload of its own').toBe(second?.stampAtStart);
    expect(loads).toEqual([PAGE, PAGE]);
    expect((await currentDocument(page))?.n).toBe(2);
    const here = new URL(page.url());
    expect(here.pathname + here.search).toBe(PAGE);
  });
});

// The gateway-session half. A dead session reaches /login with the page
// as ?next= (the gateway's login_redirect for a document, the fetch
// guard for a read — readonly-session.mocked.spec.ts pins the latter);
// these pin that sign-in RETURNS there, through the door David uses.
test.describe('sign-in returns to the page', () => {
  const LOGIN = `/login?next=${encodeURIComponent(PAGE)}`;

  test('the passkey door carries the page to the IdP', async ({ page }) => {
    await healthyBackend(page);
    await page.route(/\/api\/auth\/oidc\/available$/, (r) => json(r, { enabled: true }));
    await page.goto(LOGIN);
    await expect(page.getByRole('link', { name: /Algedonic ID/ })).toHaveAttribute(
      'href',
      `/api/auth/oidc/login?next=${encodeURIComponent(PAGE)}`,
    );
  });

  test('back from the IdP signed in, /login sends him on to the page, not /', async ({ page }) => {
    await healthyBackend(page);
    // The gateway's callback lands on /login?next=<page> with the
    // session set; LoginPage's own probe now answers with an employee.
    await page.route(/\/api\/auth\/me$/, (r) =>
      json(r, { username: 'david', employee_id: OPERATOR.id, role: OPERATOR.role }));
    const loads = documentLoads(page);
    await page.goto(LOGIN);
    await page.waitForURL((u) => u.pathname === `/ux/jobs/${JOB_ID}`);
    const here = new URL(page.url());
    expect(here.pathname + here.search).toBe(PAGE);
    await expect(page.getByText('The page David was reading').first()).toBeVisible();
    expect(loads).toEqual([LOGIN, PAGE]);
  });

  test('a hostile next is refused: signed in, he goes home', async ({ page }) => {
    await healthyBackend(page);
    await page.route(/\/api\/auth\/me$/, (r) =>
      json(r, { username: 'david', employee_id: OPERATOR.id, role: OPERATOR.role }));
    await page.goto(`/login?next=${encodeURIComponent('//evil.example/x')}`);
    await page.waitForURL((u) => u.pathname === '/');
    expect(new URL(page.url()).host).toBe(new URL(LOGIN, page.url()).host);
  });
});
