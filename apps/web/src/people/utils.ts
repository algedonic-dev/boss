// Pure helpers on employee data. Ported from apps/web/src/people/utils.ts.

import type { Certification, Employee, EmployeeId } from './types';
import { appNow } from '@boss/web-kit/sim-clock';

export function tenureYears(employee: Employee, today: Date = appNow()): number {
  // Identity-first: no hire_date yet (an un-onboarded record) means no
  // measurable tenure.
  if (!employee.hire_date) return 0;
  return (
    (today.getTime() - new Date(employee.hire_date).getTime()) /
    (1000 * 60 * 60 * 24 * 365)
  );
}

export function directReports(
  managerId: EmployeeId,
  employees: ReadonlyArray<Employee>,
): Employee[] {
  return employees.filter((e) => e.manager_id === managerId);
}

export function expiringCerts(
  daysAhead: number,
  employees: ReadonlyArray<Employee>,
  today: Date = appNow(),
): Array<{ employee: Employee; cert: Certification }> {
  const cutoff = new Date(today);
  cutoff.setDate(cutoff.getDate() + daysAhead);
  const out: Array<{ employee: Employee; cert: Certification }> = [];
  for (const e of employees) {
    for (const c of e.certifications) {
      if (!c.expires_on) continue;
      const exp = new Date(c.expires_on);
      if (exp >= today && exp <= cutoff) out.push({ employee: e, cert: c });
    }
  }
  return out;
}
