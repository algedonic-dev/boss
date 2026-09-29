// The departments registry read, reduced to what a page may say about
// it — pure, deliberately free of runes, so `bun test` can load it (the
// classify.ts precedent: a module-level $state makes a module
// unloadable outside the Svelte compiler).
//
// WHY (backlog 720d6345, 2026-09-28). `departments()` answers [] on a
// failed read exactly as it does while loading, and `departmentRoster()`
// answers null for both, so neither can tell the policy flyout that its
// department scopes are missing because the read FAILED. That is the
// question `departmentsReadFailedOf` answers.

import type { Department } from '../nav';

/// What the loader in departments.svelte.ts holds.
export type DepartmentsState =
  | { kind: 'loading' }
  | { kind: 'ready'; rows: ReadonlyArray<Department> }
  | { kind: 'error' };

/// True only once the read has answered with a failure. Loading is not
/// a failure: nothing has answered, so nothing may be claimed about it.
export function departmentsReadFailedOf(st: DepartmentsState): boolean {
  return st.kind === 'error';
}
