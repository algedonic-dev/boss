// The page survives sign-in (backlog 7d70cad6): every door off /login
// carries `next`, and only an in-app path is ever admitted.

import { describe, expect, test } from 'bun:test';
import { oidcLoginHref, safeNext } from './loginNext';

const OIDC_LOGIN = '/api/auth/oidc/login';

describe('safeNext', () => {
  test('the page a dead session came from', () => {
    expect(safeNext('?next=%2Fux%2Fjobs%2Fabc%3Ftab%3Dsteps')).toBe('/ux/jobs/abc?tab=steps');
  });

  test('the gateway spelling (slashes unescaped) reads the same', () => {
    expect(safeNext('?next=/ux/jobs/abc%3Ftab%3Dsteps')).toBe('/ux/jobs/abc?tab=steps');
  });

  test('no next, or one that is not an in-app path, is home', () => {
    for (const search of [
      '',
      '?next=',
      '?next=https%3A%2F%2Fevil.example',
      '?next=%2F%2Fevil.example',
      '?next=%2F%5Cevil.example',
      '?next=%2F%09%2Fevil.example',
      '?next=javascript%3Aalert(1)',
    ]) {
      expect(safeNext(search)).toBe('/');
    }
  });
});

describe('oidcLoginHref', () => {
  test('the passkey door carries the page through the IdP', () => {
    expect(oidcLoginHref('?next=%2Fux%2Fjobs%2Fabc%3Ftab%3Dsteps')).toBe(
      `${OIDC_LOGIN}?next=%2Fux%2Fjobs%2Fabc%3Ftab%3Dsteps`,
    );
  });

  test('a value safeNext refuses is never sent to the gateway', () => {
    expect(oidcLoginHref('?next=%2F%2Fevil.example')).toBe(OIDC_LOGIN);
    expect(oidcLoginHref('')).toBe(OIDC_LOGIN);
  });
});
