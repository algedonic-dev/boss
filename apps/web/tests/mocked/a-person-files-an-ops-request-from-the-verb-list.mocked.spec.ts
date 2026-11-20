// A person files an ops request from the verb list, on the Estate page
// (backlog 7aef2405; David 2026-11-07 ~15:05Z: "In general, I just want
// to be using the website UI for my controls"). On 2026-11-07 the gate
// volume move was filed from the pod shell, line by line, because the
// website filed only the write of a plan someone had already filed. The
// Estate page's section 05 lists the allowlist (GET /api/jobs/ops-verbs);
// the person picks a verb, its host and its params, and the form POSTs
// {host, verb, args} — nothing else — to the jobs API, which files the
// packet `boss ops` files, as them. A bad param is the server's to refuse,
// and the refusal is shown verbatim with nothing linked.

import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { pageAt } from '../fixtures/station-tree';

const PATH = pageAt('system-estate');
const FILED = 'req-rebuild-1';
const PVC = 'pvc-0a1b2c3d-0000-4000-8000-00000000abcd';
const REPLICA = `${PVC}-r-1234abcd`;

const VERBS = {
  verbs: [
    {
      name: 'plan-a-failed-replica-rebuild', about: 'read-only plan of a failed replica rebuild', hosts: ['forge'],
      params: [
        { name: 'pvc', pattern: '^pvc-[0-9a-f-]{36}$', one_of: null, default: null, optional: false, max: null },
        { name: 'replica', pattern: '^pvc-[0-9a-f-]{36}-r-[0-9a-f]{8}$', one_of: null, default: null, optional: false, max: null },
      ],
      mutating: false, requires_approval: false, requires_review: false, approvers: [],
      plan_verb: null, plan_for: 'rebuild-a-failed-replica',
    },
    {
      name: 'rebuild-a-failed-replica', about: 'MUTATING — delete one failed replica so the volume rebuilds it',
      hosts: ['forge'],
      params: [
        { name: 'pvc', pattern: '^pvc-[0-9a-f-]{36}$', one_of: null, default: null, optional: false, max: null },
        { name: 'replica', pattern: '^pvc-[0-9a-f-]{36}-r-[0-9a-f]{8}$', one_of: null, default: null, optional: false, max: null },
      ],
      mutating: true, requires_approval: true, requires_review: false, approvers: ['emp-david'],
      plan_verb: 'plan-a-failed-replica-rebuild', plan_for: null,
    },
  ],
};

const json = (r: Route, b: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

/** The Estate page with the verb list served and the filing door
 *  answering as told; returns every body POSTed to the door. */
async function mocks(page: Page, post: { status: number; body: unknown }): Promise<unknown[]> {
  const posted: unknown[] = [];
  await installSmokeMocks(page);
  await page.route(/\/api\/jobs\/ops-verbs$/, (r) => json(r, VERBS));
  await page.route(/\/api\/jobs\/file-an-ops-request$/, (r) => {
    posted.push(JSON.parse(r.request().postData() ?? 'null'));
    return json(r, post.body, post.status);
  });
  await mountPage(page, PATH, { titleMatch: /The estate/ });
  return posted;
}

test('a person picks a verb, fills its params and files it as {host, verb, args}, then sees the packet linked', async ({ page }) => {
  const posted = await mocks(page, { status: 201, body: { id: FILED } });
  const form = page.getByTestId('file-an-ops-request');
  await expect(form).toContainText('Filing is not approving.');

  await form.getByLabel('verb', { exact: true }).selectOption('rebuild-a-failed-replica');
  await expect(form).toContainText('MUTATING');
  await expect(form.getByLabel('host', { exact: true })).toHaveValue('forge');
  await expect(form).toContainText('It waits on a passkey approval by emp-david.');
  await expect(form).toContainText('matches ^pvc-[0-9a-f-]{36}$ · required');
  await expect(form.getByLabel('plan_sha256')).toHaveCount(0);
  await form.getByLabel('pvc', { exact: true }).fill(PVC);
  await form.getByLabel('replica', { exact: true }).fill(REPLICA);
  await form.getByRole('button', { name: 'File the request' }).click();

  await expect.poll(() => posted.length).toBe(1);
  expect(posted[0]).toEqual({ host: 'forge', verb: 'rebuild-a-failed-replica', args: [PVC, REPLICA] });
  const link = form.getByRole('link', { name: FILED });
  await expect(link).toBeVisible();
  await expect(link).toHaveAttribute('href', new RegExp(`/jobs/${FILED}$`));
  // The filed call cleared its fields: a second press files nothing new.
  await expect(form.getByLabel('pvc', { exact: true })).toHaveValue('');
});

test('a bad param is refused by the server, shown verbatim, and nothing is linked', async ({ page }) => {
  const why = 'REFUSED — arg `pvc` value `../../etc` does not match ^pvc-[0-9a-f-]{36}$';
  const posted = await mocks(page, { status: 422, body: { error: why } });
  const form = page.getByTestId('file-an-ops-request');

  await form.getByLabel('verb', { exact: true }).selectOption('rebuild-a-failed-replica');
  await form.getByLabel('pvc', { exact: true }).fill('../../etc');
  await form.getByLabel('replica', { exact: true }).fill(REPLICA);
  await form.getByRole('button', { name: 'File the request' }).click();

  await expect(form.getByRole('alert')).toContainText('HTTP 422');
  await expect(form.getByRole('alert')).toContainText(why);
  expect(posted).toEqual([{ host: 'forge', verb: 'rebuild-a-failed-replica', args: ['../../etc', REPLICA] }]);
  await expect(form.getByRole('link', { name: FILED })).toHaveCount(0);
});

test('a plan verb says which write it previews, so the person knows the next press is on its page', async ({ page }) => {
  await mocks(page, { status: 201, body: { id: FILED } });
  const form = page.getByTestId('file-an-ops-request');
  await form.getByLabel('verb', { exact: true }).selectOption('plan-a-failed-replica-rebuild');
  await expect(form).toContainText('This is the read-only plan of rebuild-a-failed-replica');
  await expect(form).toContainText('it needs no approval');
});

test('a verb list the server cannot give is said, never drawn as an empty allowlist', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(/\/api\/jobs\/ops-verbs$/, (r) => json(r, { error: 'jobs upstream unavailable' }, 503));
  await mountPage(page, PATH, { titleMatch: /The estate/ });
  const form = page.getByTestId('file-an-ops-request');
  await expect(form.getByRole('alert')).toContainText("Couldn't read the verb list");
  await expect(form.getByRole('button', { name: 'File the request' })).toHaveCount(0);
});
