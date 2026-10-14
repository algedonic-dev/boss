import { expect, test } from './_test';
import { installSmokeMocks } from './_smokeMocks';

test('workstations lead with the job and truthful recorded activity on a phone', async ({ page }, testInfo) => {
  await installSmokeMocks(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.route(/\/api\/jobs\?kind=agent-run&/, (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ data: [{ id: 'run-one', title: 'Builder run', metadata: { packet: 'job-one', agent: 'agent-codex', step: 'build', opened_at: '2026-10-13T20:00:00Z' }, steps: [{ spec_slug: 'building', title: 'Building', status: 'active' }] }], total: 1 }) }));
  await page.route(/\/api\/jobs\/job-one$/, (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ id: 'job-one', title: 'Repair the isolation fence', steps: [{ spec_slug: 'build', title: 'Build the change', status: 'active' }, { title: 'Triage', status: 'completed', completed_at: '2026-10-13T19:55:00Z', completed_by: 'agent-codex' }] }) }));
  await page.goto('/it?at=shop-floor');
  const floor = page.getByRole('region', { name: 'Recorded workstations' });
  await expect(floor).toBeVisible();
  await expect(floor.getByRole('link', { name: 'Repair the isolation fence' })).toHaveAttribute('href', '/ux/jobs/job-one');
  await expect(floor).toContainText('Runtime activity unknown');
  await expect(floor).toContainText('Triage');
  await expect(floor).toContainText('agent-codex');
  const bounds = await floor.boundingBox();
  expect(bounds?.width).toBeLessThanOrEqual(390);
  await floor.screenshot({ path: testInfo.outputPath('shop-floor-phone.png') });
  await page.setViewportSize({ width: 1280, height: 900 });
  await floor.screenshot({ path: testInfo.outputPath('shop-floor-desktop.png') });
});

test('an unreadable job keeps its workstation and says what is unknown', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(/\/api\/jobs\?kind=agent-run&/, (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ data: [{ id: 'run-two', title: 'Waiting run', metadata: { packet: 'job-two', agent: 'agent-codex' }, steps: [{ spec_slug: 'reported', title: 'Report recorded', status: 'ready' }] }], total: 1 }) }));
  await page.route(/\/api\/jobs\/job-two$/, (route) => route.fulfill({ status: 503, body: 'unavailable' }));
  await page.goto('/it?at=shop-floor');
  const floor = page.getByRole('region', { name: 'Recorded workstations' });
  await expect(floor).toContainText('Job evidence unavailable');
  await expect(floor).toContainText('Report recorded (ready, not yet done)');
  await expect(floor.getByRole('link', { name: 'Run record' })).toHaveAttribute('href', '/ux/jobs/run-two');
});
