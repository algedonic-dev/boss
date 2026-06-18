// Vendor + procurement CRM types.
// Aligned with crates/modules/boss-inventory/src/procurement/types.rs.

export type Vendor = {
  id: string;
  // Identity-first: only `id` is guaranteed. Descriptive fields are
  // enriched after the vendor exists, so each is nullable until set.
  name: string | null;
  contact_name: string | null;
  contact_email: string | null;
  city: string | null;
  state: string | null;
  lead_time_days: number;
  payment_terms: string | null;
  category: string | null;
};

export type PurchaseOrder = {
  id: string;
  // Identity-first: a Draft PO can exist as a bare identity; vendor and
  // placement dates are set when it's placed (required-at-place).
  vendor: string | null;
  status: string;
  placed_on: string | null;
  expected_on: string | null;
  received_on: string | null;
  lines: ReadonlyArray<unknown>;
};

export type VendorInvoice = {
  id: string;
  po_id: string;
  vendor: string;
  vendor_invoice_no: string;
  amount_cents: number;
  currency: string;
  received_on: string;
  matched_on: string | null;
  approved_on: string | null;
  paid_on: string | null;
  status: string;
  discrepancy_cents: number | null;
  discrepancy_kind: string | null;
};

export type VendorContactRole =
  | 'sales-rep'
  | 'account-manager'
  | 'customer-service'
  | 'technical-support'
  | 'finance-ap'
  | 'executive';

export type VendorContact = {
  id: string;
  vendor_id: string;
  name: string;
  role: VendorContactRole;
  email: string;
  phone: string | null;
  territory: string | null;
  specialties: ReadonlyArray<string>;
  is_primary: boolean;
  relationship_start: string | null;
  notes: string | null;
  created_at: string;
  updated_at: string;
};

export type InteractionKind =
  | 'note'
  | 'call'
  | 'meeting'
  | 'email'
  | 'rfq'
  | 'negotiation'
  | 'escalation'
  | 'interaction';

export type InteractionCommitment = {
  summary: string;
  due_by: string | null;
  linked_po_id: string | null;
};

export type VendorInteraction = {
  id: string;
  vendor_id: string;
  vendor_contact_id: string | null;
  actor_id: string;
  kind: InteractionKind;
  body: string;
  commitments: ReadonlyArray<InteractionCommitment>;
  linked_po_id: string | null;
  linked_part_sku: string | null;
  linked_job_id: string | null;
  occurred_at: string;
  created_at: string;
};

export type AccountTeamRole =
  | 'primary'
  | 'backup'
  | 'escalation'
  | 'technical-liaison'
  | 'finance-contact';

export type VendorAccountTeamMember = {
  id: string;
  vendor_id: string;
  employee_id: string;
  role: AccountTeamRole;
  assigned_on: string;
  notes: string | null;
  created_at: string;
};

export type ContractKind =
  | 'master-supply'
  | 'volume-commit'
  | 'rate-card'
  | 'rebate-program'
  | 'payment-terms'
  | 'nda'
  | 'sla';

export type ContractStatus = 'draft' | 'active' | 'expired' | 'terminated';

export type VendorContract = {
  id: string;
  vendor_id: string;
  kind: ContractKind;
  title: string;
  effective_on: string;
  expires_on: string | null;
  auto_renew: boolean;
  terms: unknown;
  document_uri: string | null;
  status: ContractStatus;
  signed_by_employee_id: string | null;
  signed_at: string | null;
  notes: string | null;
  created_at: string;
  updated_at: string;
};

export const CONTACT_ROLE_LABEL: Record<VendorContactRole, string> = {
  'sales-rep': 'Sales rep',
  'account-manager': 'Account manager',
  'customer-service': 'Customer service',
  'technical-support': 'Technical support',
  'finance-ap': 'Finance / AP',
  executive: 'Executive',
};

export const INTERACTION_KIND_LABEL: Record<InteractionKind, string> = {
  note: 'Note',
  call: 'Call',
  meeting: 'Meeting',
  email: 'Email',
  rfq: 'RFQ',
  negotiation: 'Negotiation',
  escalation: 'Escalation',
  interaction: 'Interaction',
};

export const ACCOUNT_TEAM_ROLE_LABEL: Record<AccountTeamRole, string> = {
  primary: 'Primary buyer',
  backup: 'Backup',
  escalation: 'Escalation',
  'technical-liaison': 'Technical liaison',
  'finance-contact': 'Finance contact',
};

export const CONTRACT_KIND_LABEL: Record<ContractKind, string> = {
  'master-supply': 'Master supply agreement',
  'volume-commit': 'Volume commit',
  'rate-card': 'Rate card',
  'rebate-program': 'Rebate program',
  'payment-terms': 'Payment terms',
  nda: 'NDA',
  sla: 'SLA',
};
