//! The owning metadata transaction retains the verified signer through its
//! actual write boundary. HTTP prechecks alone cannot cover a blocked SQL write.
use crate::port::{JobsError, StepVersion};
use boss_core::{actor::ActorId, publisher::EventStamp, session_claims::VerifiedSession};

#[derive(Clone)]
pub struct SignerWriteGuard {
    session: VerifiedSession,
    version: StepVersion,
    actor: ActorId,
    job: boss_core::job::Job,
}
impl SignerWriteGuard {
    pub(crate) fn new(
        session: VerifiedSession,
        version: StepVersion,
        job: boss_core::job::Job,
    ) -> Result<Self, JobsError> {
        let actor: ActorId = session
            .policy_id()
            .parse()
            .map_err(|_| JobsError::Storage("signer identity malformed".into()))?;
        if !actor.is_human() {
            return Err(JobsError::Storage("signer identity is not a person".into()));
        }
        Ok(Self {
            session,
            version,
            actor,
            job,
        })
    }
    pub(crate) fn job(&self) -> &boss_core::job::Job {
        &self.job
    }
    pub(crate) fn check_job(
        &self,
        current: Option<&boss_core::job::Job>,
        stamp: &EventStamp,
    ) -> Result<(), JobsError> {
        self.check_before_dispatch(stamp)?;
        if current != Some(&self.job) {
            return Err(JobsError::SignerWriteRefused {
                reason: "packet changed after its sign-off scope was judged".into(),
            });
        }
        Ok(())
    }
    pub(crate) fn check_row(
        &self,
        version: StepVersion,
        id: &boss_core::job::StepId,
        stamp: &EventStamp,
    ) -> Result<(), JobsError> {
        self.check_before_dispatch(stamp)?;
        if version != self.version {
            return Err(JobsError::StepChanged { id: *id });
        }
        Ok(())
    }
    pub(crate) fn check_before_dispatch(&self, stamp: &EventStamp) -> Result<(), JobsError> {
        if self.session.claims().expiry <= boss_core::presence::now_epoch()
            || stamp.actor() != &self.actor
        {
            return Err(JobsError::SignerWriteRefused {
                reason: "verified signer expired or event actor changed before its metadata write"
                    .into(),
            });
        }
        Ok(())
    }
}
