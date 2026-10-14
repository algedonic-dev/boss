import type { Vendor } from '../../src/vendors/types';

// Synthetic native DTO: descriptive Option fields serialize as null.
// One identity supplies both the shared crawl URL and its answered row.
export const CRAWL_VENDOR: Readonly<Vendor> = {
  id: 'vnd-crawl-001', name: 'Crawl vendor', contact_name: null,
  contact_email: null, city: null, state: null, lead_time_days: 7,
  payment_terms: null, category: null, behavior: null,
};
export const VENDOR_CRAWL_PATH = `/ux/vendors/${CRAWL_VENDOR.id}`;
