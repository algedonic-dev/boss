// Vendor CRM fetch helpers — plain async functions, not hooks.

import { failedRead, okRead, readStateOfResponse, type ReadState } from '../data/readState';
import type {
  VendorAccountTeamMember,
  VendorContact,
  VendorContract,
  VendorInteraction,
} from './types';

const API_BASE = '/api/inventory/vendors';

/// A CRM list and whether the read behind it worked. Until backlog
/// 865d3d51 a refusal, a non-list body and a network error each became
/// `[]`, so a vendor CRM outage read "No contacts captured yet." on the
/// vendor page; the rows alone cannot tell "none" from "unknown", so
/// the outcome travels beside them (data/readState.ts).
export type CrmList<T> = Readonly<{ rows: T[]; read: ReadState }>;

async function fetchCrmList<T>(
  vendorId: string | null | undefined,
  path: string,
): Promise<CrmList<T>> {
  // No vendor resolved: there is nothing to read, and nothing is claimed
  // — the page renders no CRM section without a vendor.
  if (!vendorId) return { rows: [], read: okRead };
  const url = `${API_BASE}/${encodeURIComponent(vendorId)}/${path}`;
  try {
    const r = await fetch(url);
    const read = readStateOfResponse(url, r);
    if (read.kind !== 'ok') return { rows: [], read };
    const body: unknown = await r.json();
    return Array.isArray(body)
      ? { rows: body as T[], read }
      : { rows: [], read: failedRead(`${url}: the answer was not a list`) };
  } catch (e) {
    return { rows: [], read: failedRead(`${url}: ${e instanceof Error ? e.message : String(e)}`) };
  }
}

export function loadVendorContacts(vendorId: string | null | undefined) {
  return fetchCrmList<VendorContact>(vendorId, 'contacts');
}
export function loadVendorInteractions(vendorId: string | null | undefined) {
  return fetchCrmList<VendorInteraction>(vendorId, 'interactions');
}
export function loadVendorAccountTeam(vendorId: string | null | undefined) {
  return fetchCrmList<VendorAccountTeamMember>(vendorId, 'account-team');
}
export function loadVendorContracts(vendorId: string | null | undefined) {
  return fetchCrmList<VendorContract>(vendorId, 'contracts');
}
