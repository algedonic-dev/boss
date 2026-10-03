//! What the knowledge base refuses before it writes — one function both
//! `KbRepository` adapters call, so a model the tables cannot hold is
//! refused the same way on each.
//!
//! WHY (backlog be459ab9, found by the adapters-agree suite,
//! 2026-10-01): every rule here is a constraint the Postgres tables
//! already enforce — a CHECK on `asset_models`, `asset_failure_modes`
//! or `parts`, a satellite primary key, TEXT and JSONB refusing a NUL
//! byte. Postgres answered each with its constraint or encoding error
//! as `Storage` (a 500) and the in-memory double STORED the value. Both
//! now answer `BadRequest` naming the field (a 422 at the HTTP layer),
//! which is what the port's own variant says caller-supplied data that
//! fails validation is.

use std::collections::HashSet;

use crate::port::KbError;
use crate::types::AssetModel;

fn bad(field: &str, why: &str) -> KbError {
    KbError::BadRequest(format!("{field} {why}"))
}

fn json_holds_nul(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::String(s) => s.contains('\0'),
        serde_json::Value::Array(a) => a.iter().any(json_holds_nul),
        serde_json::Value::Object(o) => {
            o.iter().any(|(k, v)| k.contains('\0') || json_holds_nul(v))
        }
        _ => false,
    }
}

/// Refuse a NUL byte in any of `fields` (TEXT rejects one).
fn refuse_nul<'a>(fields: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<(), KbError> {
    match fields.into_iter().find(|(_, v)| v.contains('\0')) {
        Some((f, _)) => Err(bad(f, "holds a NUL byte, which cannot be stored")),
        None => Ok(()),
    }
}

/// Refuse a key named twice in one satellite list (its primary key).
fn refuse_repeat<'a>(field: &str, keys: impl IntoIterator<Item = &'a str>) -> Result<(), KbError> {
    let mut seen = HashSet::new();
    match keys.into_iter().find(|k| !seen.insert(*k)) {
        Some(k) => Err(bad(field, &format!("names {k:?} twice"))),
        None => Ok(()),
    }
}

/// `length(currency) = 3`, counted in characters as Postgres counts.
fn refuse_currency(field: &str, currency: &str) -> Result<(), KbError> {
    if currency.chars().count() == 3 {
        Ok(())
    } else {
        Err(bad(
            field,
            &format!("must be 3 characters, got {currency:?}"),
        ))
    }
}

fn refuse_outside(field: &str, v: i64, lo: i64, hi: i64) -> Result<(), KbError> {
    if (lo..=hi).contains(&v) {
        Ok(())
    } else {
        Err(bad(
            field,
            &format!("must be between {lo} and {hi}, got {v}"),
        ))
    }
}

/// Refuse `update_model(sku, model)` whose body names another SKU: the
/// port replaces the model AT `sku`. Postgres used to upsert the body's
/// SKU (a second model) while deleting the path's satellites, and the
/// double held two rows under one SKU (backlog be459ab9).
pub fn refuse_moved_sku(sku: &str, model: &AssetModel) -> Result<(), KbError> {
    if model.sku == sku {
        Ok(())
    } else {
        Err(bad(
            "sku",
            &format!(
                "in the body ({:?}) differs from the model updated ({sku:?})",
                model.sku
            ),
        ))
    }
}

/// Every refusal a model write owes before it touches a row.
pub fn refuse_unstorable(m: &AssetModel) -> Result<(), KbError> {
    let c = &m.commerce;
    let s = &m.service;
    refuse_nul([
        ("sku", m.sku.as_str()),
        ("name", &m.name),
        ("manufacturer", &m.manufacturer),
        ("category", m.category.as_str()),
        ("currency", &c.currency),
        ("tagline", &c.tagline),
        ("description", &c.description),
        ("hero_image", c.hero_image.as_deref().unwrap_or("")),
        ("power_requirements", &m.physical.power_requirements),
        (
            "clearance_id",
            m.regulatory.clearance_id.as_deref().unwrap_or(""),
        ),
        (
            "current_firmware",
            m.current_firmware.as_deref().unwrap_or(""),
        ),
    ])?;
    refuse_nul(c.use_cases.iter().map(|u| ("use_cases", u.as_str())))?;
    refuse_nul(s.common_failure_modes.iter().flat_map(|f| {
        [&f.code, &f.name, &f.typical_fix].map(|v| ("common_failure_modes", v.as_str()))
    }))?;
    refuse_nul(s.pm_checklist.iter().map(|i| ("pm_checklist", i.as_str())))?;
    refuse_nul(m.spare_parts.iter().flat_map(|p| {
        [&p.part_sku, &p.name, &p.description, &p.currency].map(|v| ("spare_parts", v.as_str()))
    }))?;
    refuse_nul(m.consumables.iter().flat_map(|p| {
        [&p.part_sku, &p.name, &p.description, &p.currency].map(|v| ("consumables", v.as_str()))
    }))?;
    refuse_nul(m.documents.iter().flat_map(|d| {
        [
            d.kind.as_str(),
            &d.title,
            &d.url,
            d.version.as_deref().unwrap_or(""),
            d.audience.as_str(),
        ]
        .map(|v| ("documents", v))
    }))?;
    if json_holds_nul(&m.extras) {
        return Err(bad("extras", "holds a NUL byte, which cannot be stored"));
    }

    // The CHECKs of infra/postgres/schema/20-catalog.sql.
    refuse_outside("model_year", m.model_year.into(), 1980, 2100)?;
    refuse_currency("currency", &c.currency)?;
    refuse_outside(
        "regulator_device_class",
        m.regulatory.regulator_device_class.into(),
        1,
        3,
    )?;
    refuse_outside(
        "preventive_maintenance_interval_months",
        s.preventive_maintenance_interval_months.into(),
        1,
        i64::from(u8::MAX),
    )?;
    refuse_outside(
        "calibration_interval_months",
        s.calibration_interval_months.into(),
        1,
        i64::from(u8::MAX),
    )?;
    refuse_outside("required_skill_level", s.required_skill_level.into(), 1, 5)?;
    // NaN fails `BETWEEN 0 AND 1` in Postgres (NaN sorts above every
    // number) and fails `contains` here.
    if let Some(f) = s
        .common_failure_modes
        .iter()
        .find(|f| !(0.0..=1.0).contains(&f.frequency))
    {
        return Err(bad(
            "frequency",
            &format!(
                "of failure mode {:?} must be between 0 and 1, got {}",
                f.code, f.frequency
            ),
        ));
    }
    for p in &m.spare_parts {
        refuse_currency("currency", &p.currency)?;
    }
    for p in &m.consumables {
        refuse_currency("currency", &p.currency)?;
    }

    // The column WIDTHS (backlog e9ff7ccb): `lead_time_days` (u16) is
    // SMALLINT on `asset_models` and `parts`, bound `as i16`, so 40000
    // was STORED as -25536 — right through the port, wrong to every SQL
    // reader; a `pm_checklist` position is a SMALLINT `sort_order`, so a
    // list past 32767 items failed as Storage; `treatments_per_unit`
    // (u32) is INTEGER, bound `as i32`.
    let smallint = i64::from(i16::MAX);
    if let Some(d) = c.lead_time_days {
        refuse_outside("lead_time_days", d.into(), 0, smallint)?;
    }
    for p in &m.spare_parts {
        refuse_outside("lead_time_days", p.lead_time_days.into(), 0, smallint)?;
    }
    let items = i64::try_from(s.pm_checklist.len()).unwrap_or(i64::MAX);
    if items > smallint {
        return Err(bad(
            "pm_checklist",
            &format!("holds {items} items; at most {smallint} can be stored"),
        ));
    }
    for p in &m.consumables {
        if let Some(t) = p.treatments_per_unit {
            refuse_outside("treatments_per_unit", t.into(), 0, i64::from(i32::MAX))?;
        }
    }

    // The satellites' primary keys.
    refuse_repeat("use_cases", c.use_cases.iter().map(String::as_str))?;
    refuse_repeat(
        "common_failure_modes",
        s.common_failure_modes.iter().map(|f| f.code.as_str()),
    )?;
    refuse_repeat(
        "spare_parts",
        m.spare_parts.iter().map(|p| p.part_sku.as_str()),
    )?;
    refuse_repeat(
        "consumables",
        m.consumables.iter().map(|p| p.part_sku.as_str()),
    )?;
    Ok(())
}
