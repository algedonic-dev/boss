//! Versioned, non-physical catalog rows. The legacy goods catalog and
//! its conserved inventory keep their existing port and event history.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PriceBasis {
    OneTime,
    Monthly,
    /// `amount_cents` is the exclusive minimum, not a charged amount.
    PrepaidMin,
    PercentOfSpend,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkuPrice {
    pub amount_cents: i64,
    pub currency: String,
    pub basis: PriceBasis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkuInclude {
    pub code: String,
    pub text: String,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<boss_core::primitives::ClassRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkuVersion {
    pub sku: String,
    pub name: String,
    /// A tenant-defined Class code, never a closed product-kind enum.
    pub kind: String,
    pub description: String,
    pub price: SkuPrice,
    pub includes: Vec<SkuInclude>,
    pub active: bool,
    pub version: i64,
}

pub(crate) fn validate(row: &SkuVersion) -> Result<(), crate::port::ProductsError> {
    use crate::port::{ProductsError, refuse_nul};
    let fields = [
        ("sku", row.sku.as_str()),
        ("name", row.name.as_str()),
        ("kind", row.kind.as_str()),
        ("currency", row.price.currency.as_str()),
    ];
    refuse_nul(&fields)?;
    refuse_nul(&[("description", row.description.as_str())])?;
    if let Some((field, _)) = fields.iter().find(|(_, value)| value.trim().is_empty()) {
        return Err(ProductsError::Invalid(format!("{field} is empty")));
    }
    if row.version < 1 || row.price.amount_cents < 0 {
        return Err(ProductsError::Invalid(
            "version must be positive and amount_cents nonnegative".into(),
        ));
    }
    let mut codes = std::collections::BTreeSet::new();
    for included in &row.includes {
        refuse_nul(&[
            ("includes.code", &included.code),
            ("includes.text", &included.text),
        ])?;
        if included.code.trim().is_empty()
            || included.text.trim().is_empty()
            || !codes.insert(&included.code)
        {
            return Err(ProductsError::Invalid(
                "includes need distinct nonempty codes and nonempty text".into(),
            ));
        }
        if let Some(reference) = &included.reference {
            refuse_nul(&[
                ("includes.ref.subject_kind", &reference.subject_kind),
                ("includes.ref.code", &reference.code),
            ])?;
            if reference.subject_kind.trim().is_empty() || reference.code.trim().is_empty() {
                return Err(ProductsError::Invalid("includes.ref is empty".into()));
            }
        }
    }
    Ok(())
}

pub(crate) fn conflict(row: &SkuVersion) -> crate::port::ProductsError {
    crate::port::ProductsError::Invalid(format!(
        "SKU {} version {} is not the next immutable version",
        row.sku, row.version
    ))
}

/// Keep the strict row below the publisher's envelope keys. Both
/// identities and the registry's label come from this one row; replay
/// checks that the duplicated fields agree.
pub(crate) fn fact(row: &SkuVersion) -> serde_json::Value {
    serde_json::json!({"sku": row.sku, "version": row.version, "name": row.name, "data": row})
}
