//! `shipping.create` — POST a Shipment to `/api/shipping/shipments`.
//! Reads direction, origin, destination, carrier, tracking_number,
//! account_id, line_items from step metadata.

use super::common::{self, StepEvent};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use serde_json::json;
use std::sync::Arc;

pub struct ShippingCreate {
    client: boss_core::machine_token::Client,
    shipping_base: String,
}

impl ShippingCreate {
    pub fn new(shipping_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: crate::handlers::common::api_client(),
            shipping_base: shipping_base.into(),
        })
    }
}

#[async_trait]
impl Handler for ShippingCreate {
    fn name(&self) -> &'static str {
        "shipping.create"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let step = StepEvent::from_payload(&ctx.event_payload)?;

        let direction = step
            .metadata
            .get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or("outbound")
            .to_string();
        let origin = step
            .metadata
            .get("origin")
            .and_then(|v| v.as_str())
            .unwrap_or("brewery")
            .to_string();
        let destination = step
            .metadata
            .get("destination")
            .and_then(|v| v.as_str())
            .unwrap_or(step.subject_id)
            .to_string();
        let carrier = step
            .metadata
            .get("carrier")
            .and_then(|v| v.as_str())
            .unwrap_or("local-pickup")
            .to_string();
        let tracking_number = step
            .metadata
            .get("tracking_number")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let account_id = step
            .metadata
            .get("account_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let line_items = step
            .metadata
            .get("line_items")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
        let created_on = step.completed_on.ok_or_else(|| {
            HandlerError::Downstream("step.done payload missing completed_on".into())
        })?;

        let id = format!("ship-step-{}", step.step_id);
        let body = json!({
            "id": id,
            "direction": direction,
            "status": "label-created",
            "carrier": carrier,
            "tracking_number": tracking_number,
            "origin": origin,
            "destination": destination,
            "asset_ids": [],
            "line_items": line_items,
            "po_id": null,
            "order_id": null,
            "account_id": account_id,
            "created_on": created_on,
            "shipped_on": null,
            "estimated_delivery": null,
            "delivered_on": null,
        });

        let url = format!(
            "{}/api/shipping/shipments",
            self.shipping_base.trim_end_matches('/')
        );
        if common::post_json_or_held(&self.client, &url, &body, &ctx.rule_name).await? {
            return Ok(());
        }
        // 409: the id is held. The id is this step's own
        // (`ship-step-{step_id}`), so a redelivered `step.done` meets the
        // shipment its first delivery created — already applied, done.
        // Until 2026-10-01 the store upserted over it instead, regressing
        // a scanned shipment (backlog be459ab9). But the id being held
        // proves only that SOMETHING created it: read it back and accept
        // it only when the fields this create sets once — direction,
        // origin, destination, created_on — are this step's. Anything
        // else is a real collision and stays loud.
        let held = common::get_json(&self.client, &format!("{url}/{id}"), &ctx.rule_name).await?;
        let ours = ["id", "direction", "origin", "destination", "created_on"]
            .iter()
            .all(|k| held.get(*k) == body.get(*k));
        if ours {
            return Ok(());
        }
        Err(HandlerError::Downstream(format!(
            "shipment {id} is already held and is not the one step {} creates: \
             held {held}, this step would create {body}",
            step.step_id
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A stub shipping API holding ONE shipment: a POST of its id is
    /// refused 409 (as the store has since 2026-10-01), a POST of any
    /// other id creates it; every POST is counted.
    async fn stub_shipping(
        held: serde_json::Value,
    ) -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
        use axum::extract::Path;
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::{Json, Router, routing::get, routing::post};
        let posts: Arc<Mutex<Vec<serde_json::Value>>> = Default::default();
        let seen = posts.clone();
        let held_id = held["id"].as_str().unwrap_or_default().to_string();
        let held_for_post = held_id.clone();
        let app = Router::new()
            .route(
                "/api/shipping/shipments",
                post(move |Json(body): Json<serde_json::Value>| {
                    let seen = seen.clone();
                    let held_id = held_for_post.clone();
                    async move {
                        seen.lock().unwrap().push(body.clone());
                        if body["id"] == held_id.as_str() {
                            (StatusCode::CONFLICT, "shipment already exists").into_response()
                        } else {
                            (StatusCode::CREATED, Json(json!({"id": body["id"]}))).into_response()
                        }
                    }
                }),
            )
            .route(
                "/api/shipping/shipments/{id}",
                get(move |Path(id): Path<String>| {
                    let held = held.clone();
                    let held_id = held_id.clone();
                    async move {
                        if id == held_id {
                            Json(held).into_response()
                        } else {
                            StatusCode::NOT_FOUND.into_response()
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), posts)
    }

    fn ctx() -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: "shipping-create-on-shipment-done".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: "step.done.shipment".into(),
            event_payload: json!({
                "job_id": "job-1",
                "step_id": "step-1",
                "kind": "shipment",
                "subject_kind": "account",
                "subject_id": "acct-1",
                "completed_on": "2026-09-30",
                "metadata": {"origin": "Warehouse", "destination": "Dock 4", "carrier": "parcel-co"},
            }),
        }
    }

    /// The shipment step-1's first delivery created, since scanned
    /// in-transit by the carrier.
    fn step_ones_shipment() -> serde_json::Value {
        json!({
            "id": "ship-step-step-1",
            "direction": "outbound",
            "status": "in-transit",
            "carrier": "parcel-co",
            "tracking_number": null,
            "origin": "Warehouse",
            "destination": "Dock 4",
            "asset_ids": [],
            "line_items": [],
            "po_id": null,
            "order_id": null,
            "account_id": null,
            "created_on": "2026-09-30",
            "shipped_on": "2026-09-30",
            "estimated_delivery": null,
            "delivered_on": null,
        })
    }

    /// A redelivered `step.done` meets the shipment its first delivery
    /// created: the 409 is read as already applied, the handler
    /// completes, and it posts once — never a second create.
    #[tokio::test]
    async fn a_redelivery_against_its_own_shipment_completes_once() {
        let (base, posts) = stub_shipping(step_ones_shipment()).await;
        let h = ShippingCreate::new(base);
        h.invoke(&[], &ctx()).await.expect("already applied");
        assert_eq!(posts.lock().unwrap().len(), 1, "one POST, no retry");
    }

    /// The id is held by a shipment this step did not create (another
    /// day, another destination): a real collision, which stays an
    /// error rather than reading as done.
    #[tokio::test]
    async fn a_held_id_that_is_not_this_steps_stays_an_error() {
        let mut other = step_ones_shipment();
        other["destination"] = json!("Somewhere else");
        other["created_on"] = json!("2026-01-01");
        let (base, posts) = stub_shipping(other).await;
        let h = ShippingCreate::new(base);
        match h.invoke(&[], &ctx()).await {
            Err(HandlerError::Downstream(m)) => {
                assert!(m.contains("ship-step-step-1"), "{m}");
                assert!(m.contains("not the one step step-1 creates"), "{m}");
            }
            other => panic!("a collision must stay loud: {other:?}"),
        }
        assert_eq!(posts.lock().unwrap().len(), 1);
    }

    /// The first delivery creates, as before.
    #[tokio::test]
    async fn a_first_delivery_creates_the_shipment() {
        let (base, posts) = stub_shipping(json!({"id": "someone-else"})).await;
        let h = ShippingCreate::new(base);
        h.invoke(&[], &ctx()).await.expect("created");
        let posts = posts.lock().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0]["id"], "ship-step-step-1");
        assert_eq!(posts[0]["status"], "label-created");
    }
}
