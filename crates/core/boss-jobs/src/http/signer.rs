//! A declared signer writes through a verified gateway session, not an
//! asserted machine identity (approved f623 D1; oldest6c). No issuer lives here.
use super::*;
use crate::field_writer::CredentialedCaller;
use crate::field_writer::SIGNER_WRITER;
use boss_core::session_claims::{VerifiedSession, unique_cookie, verify};

pub(super) struct Signer {
    session: VerifiedSession,
    pub user: boss_policy_client::User,
}

pub(super) async fn resolve<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    step: &Step,
    keys: impl IntoIterator<Item = impl AsRef<str>>,
    headers: &axum::http::HeaderMap,
    caller: Option<&CredentialedCaller>,
    asked_by: &str,
    door: &str,
) -> Result<Option<Signer>, Response> {
    let keys: Vec<_> = keys
        .into_iter()
        .map(|key| key.as_ref().to_owned())
        .collect();
    if !step
        .fields
        .iter()
        .any(|field| field.writer.as_deref() == Some(SIGNER_WRITER) && keys.contains(&field.name))
    {
        return Ok(None);
    }
    let refused = |why: &str| {
        let keys: Vec<_> = step
            .fields
            .iter()
            .filter(|field| {
                field.writer.as_deref() == Some(SIGNER_WRITER) && keys.contains(&field.name)
            })
            .map(|field| {
                (
                    crate::field_writer::ReservedKey {
                        key: field.name.clone(),
                        writer: SIGNER_WRITER.into(),
                    },
                    why.into(),
                )
            })
            .collect();
        (
            StatusCode::CONFLICT,
            Json(crate::field_writer::refusal_body(
                &step.id.to_string(),
                &step.title,
                door,
                asked_by,
                caller,
                &keys,
            )),
        )
            .into_response()
    };
    if caller.is_some() {
        return Err(refused(
            "a runner credential cannot substitute for a signer session",
        ));
    }
    verify_request(state, headers)
        .await
        .map(Some)
        .map_err(refused)
}

pub(super) async fn preflight_user<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    headers: &axum::http::HeaderMap,
) -> Option<boss_policy_client::User> {
    verify_request(state, headers)
        .await
        .ok()
        .map(|signer| signer.user)
}

pub(super) async fn policy_first<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
    headers: &axum::http::HeaderMap,
) -> Result<(boss_policy_client::Scope, Option<Response>), Response> {
    let refusal = match state.policy.ask(user, controls::UPDATE_STEP).await {
        Ok(Decision::Allow { scope }) => return Ok((scope, None)),
        Ok(Decision::Deny { reason }) => (StatusCode::FORBIDDEN, reason).into_response(),
        Err(error) => error.into_response(),
    };
    // No protected row is read before an actual Update grant. A verified
    // session may supply that grant when the raw header names somebody else.
    if let Some(signed) = preflight_user(state, headers).await {
        match state.policy.ask(&signed, controls::UPDATE_STEP).await {
            Ok(Decision::Allow { scope }) => return Ok((scope, Some(refusal))),
            Ok(Decision::Deny { .. }) => {}
            Err(error) => return Err(error.into_response()),
        }
    }
    Err(refusal)
}

async fn verify_request<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    headers: &axum::http::HeaderMap,
) -> Result<Signer, &'static str> {
    let cookies = headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .map(|value| value.to_str())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "malformed session cookie header")?;
    let value = unique_cookie(cookies)
        .map_err(|_| "ambiguous session cookie")?
        .ok_or("no gateway session cookie")?;
    let key = state
        .presence_key
        .as_deref()
        .ok_or("session verifier unavailable")?
        .get()
        .await
        .ok_or("session verifier unavailable")?;
    let session = verify(value, key, boss_core::presence::now_epoch())
        .map_err(|_| "gateway session did not verify")?;
    let wire = session.claims();
    let user = boss_policy_client::User {
        id: session.policy_id().into(),
        role: session.effective_role().into(),
        access_tier: if session.access_tier() == "operator" {
            boss_policy_client::AccessTier::Operator
        } else {
            boss_policy_client::AccessTier::User
        },
        department: wire.department.clone(),
        territory_account_ids: wire.territory_account_ids.clone(),
        direct_report_ids: wire.direct_report_ids.clone(),
    };
    if !user.ambient_actor().is_some_and(|actor| actor.is_human()) {
        return Err("the authenticated session is not a person signer");
    }
    Ok(Signer { session, user })
}

impl Signer {
    pub(super) fn write_guard(
        &self,
        version: crate::port::StepVersion,
        job: &Job,
    ) -> Result<crate::signer_write::SignerWriteGuard, crate::port::JobsError> {
        crate::signer_write::SignerWriteGuard::new(self.session.clone(), version, job.clone())
    }
    pub(super) fn before_write(&self) -> Result<(), crate::port::JobsError> {
        if self.session.claims().expiry <= boss_core::presence::now_epoch() {
            return Err(crate::port::JobsError::SignerWriteRefused {
                reason: "signer session expired before its write".into(),
            });
        }
        Ok(())
    }
    pub(super) async fn caller<R: JobsRepository, B: EventBus>(
        &self,
        state: &JobsApiState<R, B>,
        step: &Step,
        job: &Job,
    ) -> Result<CredentialedCaller, Response> {
        // Required roles are protocol data. Update permission or an assignee
        // does not supply the independent sign-off grant or its packet scope.
        for role in &step.sign_offs_required {
            match state
                .policy
                .check(
                    &self.user,
                    Action::SignOff,
                    Resource::new(format!("step-signoff:{role}")),
                )
                .await
            {
                Ok(Decision::Allow { scope }) if scope_matches(&self.user, &scope, job) => {
                    self.before_write().map_err(|error| {
                        (StatusCode::CONFLICT, error.to_string()).into_response()
                    })?;
                    return Ok(CredentialedCaller {
                        principal: SIGNER_WRITER.into(),
                        actor_id: self.user.id.clone(),
                        host: None,
                    });
                }
                Ok(_) => {}
                Err(error) => return Err(error.into_response()),
            }
        }
        Err((
            StatusCode::CONFLICT,
            "the authenticated session holds no required sign-off role within this packet's scope",
        )
            .into_response())
    }
}
