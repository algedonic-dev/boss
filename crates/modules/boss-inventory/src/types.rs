//! Domain types for inventory management.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

fn default_currency() -> String {
    "USD".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InventoryItem {
    pub part_sku: String,
    pub bin: String,
    pub on_hand: u32,
    pub allocated: u32,
    pub reorder_point: u32,
    pub reorder_qty: u32,
    pub trailing_90d_usage: u32,
    /// Weighted moving-average unit cost in cents. Updated on
    /// each receive (see `port::receive_part_at`). Consumed by
    /// `PartsConsumeEmitter` to compute the COGS amount when
    /// ingredients are drawn down in production.
    #[serde(default)]
    pub avg_cost_cents: i64,
    /// The supplier's agreed unit price in cents — what the vendor
    /// charges us. Data (seeded from `parts.toml`, operator-editable),
    /// never computed. PO lines are priced from this at placement;
    /// `avg_cost_cents` is *our* cost and emerges from receipts at PO
    /// prices. `None` = no agreed price; auto-restock refuses to
    /// place an unpriced PO.
    #[serde(default)]
    pub vendor_price_cents: Option<i64>,
    /// Category of vendor that supplies this part — matches
    /// `vendors.category`. `primary_vendor_for_part` uses it to pick
    /// a category-appropriate supplier when the part has no PO
    /// history yet (the first auto-restock). Kept as data: seeded
    /// per-SKU from the tenant's `parts.toml`. `None` for parts that
    /// declare no category (resolution then yields no vendor).
    #[serde(default)]
    pub vendor_category: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Vendor {
    pub id: String,
    /// Identity-first: only `id` is required to create a vendor.
    /// Descriptive fields are enriched after the vendor exists, so each
    /// is nullable until set. A vendor with no `category` yet isn't an
    /// auto-restock target (the dispatcher's `vendor_for` resolution
    /// matches on category) until it's classified — which is correct.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub contact_name: Option<String>,
    #[serde(default)]
    pub contact_email: Option<String>,
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    /// Numeric with a sane default (7d); not identity data, so it stays
    /// non-null with a schema default rather than nullable.
    pub lead_time_days: u16,
    #[serde(default)]
    pub payment_terms: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PoStatus {
    Draft,
    Submitted,
    Acknowledged,
    InTransit,
    Received,
    Closed,
}

impl PoStatus {
    /// True once the PO has been placed with the vendor — any status
    /// past `Draft`. A `Draft` PO may be a bare identity (identity-first);
    /// every placed status carries the required-at-place obligations.
    pub fn is_placed(&self) -> bool {
        !matches!(self, PoStatus::Draft)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PurchaseOrderLine {
    pub part_sku: String,
    pub qty: u32,
    pub unit_cost_cents: i64,
    #[serde(default = "default_currency")]
    pub currency: String,
}

impl From<&PurchaseOrderLine> for boss_core::primitives::Part {
    /// PO lines are AttributeParts of their parent PurchaseOrder
    /// Subject. Key is `"po_line"` so the KB view can group / label
    /// consistently across kinds.
    fn from(line: &PurchaseOrderLine) -> Self {
        boss_core::primitives::Part::attribute(
            "po_line",
            serde_json::to_value(line).expect("PurchaseOrderLine serialises"),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PurchaseOrder {
    pub id: String,
    /// Identity-first: a PO identity can exist (status `Draft`) before
    /// its vendor, lines, and dates are finalized. These are required
    /// only to *place* it (see [`PoStatus::is_placed`] +
    /// [`PurchaseOrder::validate_placement`]) — required-at-place, the
    /// procurement analogue of the Step required-at-done rule.
    #[serde(default)]
    pub vendor: Option<String>,
    pub status: PoStatus,
    #[serde(default)]
    pub placed_on: Option<NaiveDate>,
    #[serde(default)]
    pub expected_on: Option<NaiveDate>,
    pub received_on: Option<NaiveDate>,
    #[serde(default)]
    pub lines: Vec<PurchaseOrderLine>,
}

impl PurchaseOrder {
    /// Required-at-place: a PO that has been placed (any status past
    /// `Draft`) must name a vendor, carry at least one line, and record
    /// when it was placed. A `Draft` PO may be a bare identity.
    /// Returns the human-readable reason it can't be placed, or `Ok`.
    pub fn validate_placement(&self) -> Result<(), String> {
        if !self.status.is_placed() {
            return Ok(());
        }
        if self.vendor.as_deref().unwrap_or("").is_empty() {
            return Err("a placed purchase order must name a vendor".into());
        }
        if self.lines.is_empty() {
            return Err("a placed purchase order must have at least one line".into());
        }
        if self.placed_on.is_none() {
            return Err("a placed purchase order must record placed_on".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VendorInvoiceStatus {
    Received,
    Matched,
    Mismatched,
    Approved,
    Paid,
}

impl VendorInvoiceStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            VendorInvoiceStatus::Received => "received",
            VendorInvoiceStatus::Matched => "matched",
            VendorInvoiceStatus::Mismatched => "mismatched",
            VendorInvoiceStatus::Approved => "approved",
            VendorInvoiceStatus::Paid => "paid",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "received" => VendorInvoiceStatus::Received,
            "matched" => VendorInvoiceStatus::Matched,
            "mismatched" => VendorInvoiceStatus::Mismatched,
            "approved" => VendorInvoiceStatus::Approved,
            "paid" => VendorInvoiceStatus::Paid,
            _ => return None,
        })
    }
}

/// Why a vendor invoice failed the three-way match (overbilled,
/// shorted, wrong-price, wrong-qty, …). Free-text wrapper around a
/// kebab-case string; tenants extend via the Class registry under
/// `(subject_kind='vendor-invoice')`. The `vendor_invoices` row stores
/// the code; validation happens at the inventory API boundary against
/// the active Class set per docs/design/class-registry.md. Serializes
/// transparently to the bare string. The field is optional on a
/// `VendorInvoice` (a clean match carries no discrepancy).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DiscrepancyKind(pub String);

impl DiscrepancyKind {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DiscrepancyKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for DiscrepancyKind {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for DiscrepancyKind {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// One bucket in an AP-aging report: unpaid vendor invoices grouped
/// by how old they are. Parallels `boss_commerce::ArAgingBucket`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApAgingBucket {
    pub label: String,
    pub count: i64,
    pub total_cents: i64,
}

/// Full AP-aging payload: per-bucket counts + the aggregate outstanding
/// amount. Reporting currency is pinned to USD for v1; the schema has
/// a currency column on each invoice but today every row is USD.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApAging {
    pub buckets: Vec<ApAgingBucket>,
    pub total_outstanding_cents: i64,
    pub total_invoice_count: i64,
    #[serde(default = "default_currency")]
    pub currency: String,
}

/// A vendor invoice that a purchase order is matched against in the
/// three-way match workflow. The A/P specialist receives these bills,
/// compares them to the originating PO + receipt, and approves for
/// payment when everything lines up. Mismatches are flagged for
/// human review.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VendorInvoice {
    pub id: String,
    pub po_id: String,
    pub vendor: String,
    pub vendor_invoice_no: String,
    pub amount_cents: i64,
    #[serde(default = "default_currency")]
    pub currency: String,
    pub received_on: NaiveDate,
    pub matched_on: Option<NaiveDate>,
    pub approved_on: Option<NaiveDate>,
    pub paid_on: Option<NaiveDate>,
    pub status: VendorInvoiceStatus,
    pub discrepancy_cents: Option<i64>,
    pub discrepancy_kind: Option<DiscrepancyKind>,
    /// Per-SKU bill breakdown — the source of truth for the
    /// `finance.bill.approved` JE's amount. When present, the
    /// posting rule sums `qty × unit_cost_cents` across this
    /// array and validates against `amount_cents`. Legacy
    /// payloads without `lines` fall back to the lump
    /// `amount_cents` (the rule accepts either shape).
    #[serde(default)]
    pub lines: Vec<BillLine>,
}

/// One bill-line entry: per-SKU receipt cost. Used by
/// `bill_approved` to derive the JE amount and (eventually) to
/// recover the per-line cost-of-receipt that updates
/// `inventory_items.avg_cost_cents` via the receive step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BillLine {
    pub part_sku: String,
    pub qty: i64,
    pub unit_cost_cents: i64,
}

#[cfg(test)]
mod po_placement_tests {
    use super::*;

    fn po(status: PoStatus, vendor: Option<&str>, with_line: bool, placed: bool) -> PurchaseOrder {
        PurchaseOrder {
            id: "PO-1".into(),
            vendor: vendor.map(String::from),
            status,
            placed_on: placed.then(|| chrono::NaiveDate::from_ymd_opt(2025, 3, 1).unwrap()),
            expected_on: None,
            received_on: None,
            lines: if with_line {
                vec![PurchaseOrderLine {
                    part_sku: "PART-1".into(),
                    qty: 1,
                    unit_cost_cents: 100,
                    currency: "USD".into(),
                }]
            } else {
                vec![]
            },
        }
    }

    #[test]
    fn draft_po_can_be_a_bare_identity() {
        // Identity-first: a Draft needs nothing but its id.
        assert!(
            po(PoStatus::Draft, None, false, false)
                .validate_placement()
                .is_ok()
        );
    }

    #[test]
    fn placed_po_requires_vendor_lines_and_placed_on() {
        // Required-at-place: each missing field is rejected with a reason.
        assert!(
            po(PoStatus::Submitted, None, true, true)
                .validate_placement()
                .is_err()
        );
        assert!(
            po(PoStatus::Submitted, Some("v"), false, true)
                .validate_placement()
                .is_err()
        );
        assert!(
            po(PoStatus::Submitted, Some("v"), true, false)
                .validate_placement()
                .is_err()
        );
        // Complete placed PO passes.
        assert!(
            po(PoStatus::Submitted, Some("v"), true, true)
                .validate_placement()
                .is_ok()
        );
    }
}

#[cfg(test)]
mod part_conversion_tests {
    use super::*;
    use boss_core::primitives::Part;

    #[test]
    fn po_line_converts_to_attribute_part() {
        let line = PurchaseOrderLine {
            part_sku: "PART-HENE-1A".into(),
            qty: 10,
            unit_cost_cents: 5_000,
            currency: "USD".into(),
        };
        let part: Part = (&line).into();
        match part {
            Part::Attribute { key, value } => {
                assert_eq!(key, "po_line");
                assert_eq!(
                    value.get("part_sku").and_then(|v| v.as_str()),
                    Some("PART-HENE-1A"),
                );
                assert_eq!(value.get("qty").and_then(|v| v.as_i64()), Some(10));
            }
            Part::Subject { .. } => panic!("po line should be AttributePart"),
        }
    }
}
