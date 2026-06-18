// Vendor CRM fetch helpers — plain async functions, not hooks.

import type {
  VendorAccountTeamMember,
  VendorContact,
  VendorContract,
  VendorInteraction,
} from './types';

const API_BASE = '/api/inventory/vendors';

async function fetchCrmList<T>(
  vendorId: string | null | undefined,
  path: string,
): Promise<T[]> {
  if (!vendorId) return [];
  try {
    const r = await fetch(`${API_BASE}/${encodeURIComponent(vendorId)}/${path}`);
    if (!r.ok) return [];
    const body = await r.json();
    return Array.isArray(body) ? (body as T[]) : [];
  } catch {
    return [];
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
