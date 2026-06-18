//! Reorder-rule helpers — `open_po_exists`, `vendor_for`, `open_restock_exists`.
//!
//! These are the helper functions the canonical reorder-threshold rule
//! calls. The first two query the inventory-api; `open_restock_exists`
//! queries the jobs-api to dedup per-SKU on an in-flight restock Job (which
//! exists the instant it's spawned) rather than on the PO it places later.
//!
//! Implementation note: HelperResolver::call is sync because the
//! expression evaluator is sync. The matcher runs inside tokio, so
//! we use `block_in_place` + `Handle::current().block_on` to bridge
//! to async reqwest. Requires a multi-threaded tokio runtime (which
//! the dispatcher binary uses by default via #[tokio::main]).

use super::expr::{EvalError, HelperResolver, Value};

const SYSTEM_USER_HEADER: &str = r#"{"id":"automation:dispatcher","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

pub struct InventoryHelpers {
    client: reqwest::Client,
    inventory_base: String,
    jobs_base: String,
}

impl InventoryHelpers {
    pub fn new(inventory_base: impl Into<String>, jobs_base: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
                .expect("async client builds"),
            inventory_base: inventory_base.into(),
            jobs_base: jobs_base.into(),
        }
    }

    fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        helper: &str,
    ) -> Result<T, EvalError> {
        let req = self
            .client
            .get(url)
            .header("x-boss-user", SYSTEM_USER_HEADER)
            .send();
        let resp = tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(req))
            .map_err(|e| EvalError::HelperFailed {
                name: helper.to_string(),
                msg: format!("GET {url}: {e}"),
            })?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(EvalError::HelperFailed {
                name: helper.to_string(),
                msg: format!("GET {url} returned {status}"),
            });
        }
        let body = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(resp.json::<T>())
        })
        .map_err(|e| EvalError::HelperFailed {
            name: helper.to_string(),
            msg: format!("decode body: {e}"),
        })?;
        Ok(body)
    }
}

impl HelperResolver for InventoryHelpers {
    fn call(&self, name: &str, args: &[Value]) -> Result<Value, EvalError> {
        match name {
            "open_po_exists" => open_po_exists(self, args),
            "vendor_for" => vendor_for(self, args),
            "open_restock_exists" => open_restock_exists(self, args),
            other => Err(EvalError::UnknownHelper(other.to_string())),
        }
    }
}

fn first_string<'a>(args: &'a [Value], helper: &str) -> Result<&'a str, EvalError> {
    match args.first() {
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(other) => Err(EvalError::TypeError {
            expected: "string sku",
            got: other.kind(),
        }),
        None => Err(EvalError::HelperFailed {
            name: helper.to_string(),
            msg: "missing required arg".into(),
        }),
    }
}

#[derive(serde::Deserialize)]
struct OpenPoResponse {
    exists: bool,
}

fn open_po_exists(h: &InventoryHelpers, args: &[Value]) -> Result<Value, EvalError> {
    let sku = first_string(args, "open_po_exists")?;
    let url = format!(
        "{}/api/inventory/items/{sku}/open-po-exists",
        h.inventory_base.trim_end_matches('/')
    );
    let r: OpenPoResponse = h.get_json(&url, "open_po_exists")?;
    Ok(Value::Bool(r.exists))
}

#[derive(serde::Deserialize)]
struct VendorForResponse {
    vendor_id: String,
}

fn vendor_for(h: &InventoryHelpers, args: &[Value]) -> Result<Value, EvalError> {
    let sku = first_string(args, "vendor_for")?;
    let url = format!(
        "{}/api/inventory/items/{sku}/primary-vendor",
        h.inventory_base.trim_end_matches('/')
    );
    let r: VendorForResponse = h.get_json(&url, "vendor_for")?;
    Ok(Value::String(r.vendor_id))
}

#[derive(serde::Deserialize)]
struct JobsListResponse {
    data: Vec<serde_json::Value>,
}

/// True if an `ingredient-restock` Job for `part_sku` is already open.
///
/// The reorder rule dedups on this — per-SKU, on the in-flight restock Job
/// (which exists the instant it's spawned) — rather than on the open PO the
/// restock places much later (after its audit-stock step). That lag let a
/// cold-start burst of `inventory.item.consumed` events spawn dozens of
/// duplicate restocks for the same ingredient before any PO landed. Matches
/// on the Job's `metadata.part_sku` (stamped by the reorder rule via
/// `jobs.spawn`'s `metadata.<field>` args), since the restock's subject is
/// the vendor, not the part.
fn open_restock_exists(h: &InventoryHelpers, args: &[Value]) -> Result<Value, EvalError> {
    let part_sku = first_string(args, "open_restock_exists")?;
    let url = format!(
        "{}/api/jobs?kind=ingredient-restock&status=open&limit=200",
        h.jobs_base.trim_end_matches('/')
    );
    let r: JobsListResponse = h.get_json(&url, "open_restock_exists")?;
    let exists = r.data.iter().any(|j| {
        j.get("metadata")
            .and_then(|m| m.get("part_sku"))
            .and_then(|s| s.as_str())
            == Some(part_sku)
    });
    Ok(Value::Bool(exists))
}
