//! Per-category JSON-schema for `AssetModel.extras`.
//!
//! `asset_models.extras` is validated against a platform-stored
//! JSON schema: the platform stores one JSON-schema row per
//! `category_code` in
//! `asset_model_extras_schema`. On every catalog write, the
//! catalog looks up the schema for the model's category and
//! validates the `extras` blob against it. Categories without
//! a schema row pass through (the platform stays neutral — a
//! tenant who hasn't yet published a schema gets pure pass-
//! through writes).
//!
//! ## Failure mode
//!
//! Validation errors surface as [`KbError::BadRequest`] with a
//! human-readable list of pointer-style paths that failed. The HTTP
//! layer maps that to a 422; SDK callers see exactly which keys are
//! wrong.

#[cfg(feature = "postgres")]
use sqlx::PgPool;

use crate::port::KbError;

/// Validate `extras` against the JSON schema stored for `category_code`,
/// if any. Returns `Ok(())` when:
/// - no schema is registered for the category (pass-through), OR
/// - validation succeeds against the registered schema.
///
/// Returns [`KbError::BadRequest`] with all failing pointers when the
/// schema rejects the value.
#[cfg(feature = "postgres")]
pub async fn validate_extras(
    pool: &PgPool,
    category_code: &str,
    extras: &serde_json::Value,
) -> Result<(), KbError> {
    let row: Option<(serde_json::Value,)> =
        sqlx::query_as("SELECT schema FROM asset_model_extras_schema WHERE category_code = $1")
            .bind(category_code)
            .fetch_optional(pool)
            .await
            .map_err(|e| KbError::Storage(e.to_string()))?;

    let Some((schema_json,)) = row else {
        // No schema published for this category — pass-through.
        return Ok(());
    };

    let validator = jsonschema::validator_for(&schema_json).map_err(|e| {
        // Only happens if the *stored* schema is malformed. That's an
        // operator-side authoring error; surface it as a Storage
        // error so it's not blamed on the request payload.
        KbError::Storage(format!(
            "extras schema for category `{category_code}` is invalid: {e}"
        ))
    })?;

    let errors: Vec<String> = validator
        .iter_errors(extras)
        .map(|err| format!("{}: {}", err.instance_path(), err))
        .collect();
    if !errors.is_empty() {
        return Err(KbError::BadRequest(format!(
            "extras failed schema validation for category `{}`: {}",
            category_code,
            errors.join("; ")
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(feature = "postgres")]
    #[tokio::test(flavor = "multi_thread")]
    async fn passes_through_when_no_schema_registered() {
        let db = boss_testing::TestDb::new().await;
        let r = validate_extras(&db.pool, "fractional-co2", &json!({"anything": 42})).await;
        assert!(r.is_ok(), "expected pass-through, got {r:?}");
    }

    #[cfg(feature = "postgres")]
    #[tokio::test(flavor = "multi_thread")]
    async fn rejects_extras_violating_registered_schema() {
        let db = boss_testing::TestDb::new().await;
        sqlx::query(
            "INSERT INTO asset_model_extras_schema (category_code, schema) \
             VALUES ($1, $2)",
        )
        .bind("fractional-co2")
        .bind(json!({
            "type": "object",
            "required": ["wavelengths_nm"],
            "properties": {
                "wavelengths_nm": { "type": "array", "items": { "type": "integer" } }
            }
        }))
        .execute(&db.pool)
        .await
        .unwrap();

        let r = validate_extras(&db.pool, "fractional-co2", &json!({})).await;
        assert!(r.is_err(), "missing required key should fail");
        let err = r.unwrap_err();
        assert!(
            matches!(err, KbError::BadRequest(ref m) if m.contains("wavelengths_nm")),
            "got {err:?}"
        );

        let r = validate_extras(
            &db.pool,
            "fractional-co2",
            &json!({"wavelengths_nm": [10600, 532]}),
        )
        .await;
        assert!(r.is_ok(), "valid extras should pass: {r:?}");
    }

    #[cfg(feature = "postgres")]
    #[tokio::test(flavor = "multi_thread")]
    async fn malformed_schema_surfaces_as_storage_error() {
        let db = boss_testing::TestDb::new().await;
        sqlx::query(
            "INSERT INTO asset_model_extras_schema (category_code, schema) \
             VALUES ($1, $2)",
        )
        .bind("printer-mfp")
        .bind(json!({"type": "not-a-real-jsonschema-type"}))
        .execute(&db.pool)
        .await
        .unwrap();

        let r = validate_extras(&db.pool, "printer-mfp", &json!({})).await;
        assert!(matches!(r, Err(KbError::Storage(_))), "got {r:?}");
    }
}
