//! The key this jobs API verifies presence tickets with (backlog
//! 72fe3640, 2026-09-24).
//!
//! `Assurance::Presence` is granted on the gateway's signed ticket
//! (`boss_core::presence`), and the only way to know a ticket is the
//! gateway's is to check its HMAC with the gateway's key. That key is
//! derived from the session key: the gateway and this API run in the one
//! `boss` container and read the same read-only Secret mount, named by
//! `BOSS_SESSION_KEY` — so no new secret exists for this, and none was
//! invented. `PresenceTicket::decode` derives the ticket-only key from
//! what this holds (backlog 19f00800), so a session cookie presented as
//! a ticket is refused here on its signature.
//!
//! WHAT THIS PROCESS HOLDS, AND WHAT RESTS ON THE CONTAINER. This holds
//! the SESSION key itself, not only the ticket key, because the signer
//! door (`http/signer.rs`, f623e425) verifies gateway session cookies
//! with it — and an HMAC verifier can sign, so this process can mint any
//! gateway session, operator tier included. Even without that, every
//! process in the one `boss` container can read the key file. Until the
//! container is split (design c22f36c6 Q2, deferred to the hosting
//! provision protocol) a taken-over jobs API is a taken-over gateway
//! session; the ticket key's one-way derivation is what lets a split
//! hand this API presence without the session key.
//!
//! READ AT START WHEN THE FILE IS THERE; ON FIRST USE ONLY WHEN IT IS
//! NOT (design c22f36c6 item 6). The gateway reads the key once, at
//! start, so a copy read at the same pod start is the copy that matches
//! it — one read at the first claim held whatever the file said THEN,
//! and a Secret replaced in between split the two. The first-use read
//! remains for the quickstart race: every service in that launcher starts
//! in parallel and the gateway CREATES the key file when it finds none,
//! so a jobs API that found none at start reads it the first time a
//! presence claim arrives, and keeps the first valid key it reads; until
//! then each claim tries again. It is never created here — only the
//! gateway mints a key.
//!
//! ROTATION IS A SECRET REPLACE PLUS A ROLL of the `boss` Deployment, one
//! act: the roll restarts both processes and they read the same bytes.
//! The kubelet re-projects a replaced Secret in place without a restart,
//! so a replace with no roll leaves the gateway signing with the old key.
//! When a claim fails to verify, the file is read again; if it holds a
//! different key than this process read, that is said aloud — on the log
//! and in the refusal — and the claim is STILL refused, because switching
//! would not help while the gateway signs with the key it read at start.
//!
//! FAIL CLOSED. No key — unconfigured, unreadable, malformed — means no
//! ticket verifies, so no request is granted presence. There is no
//! fallback to reading the header's fields on trust: that fallback WAS
//! the defect.

use std::path::PathBuf;

use tokio::sync::OnceCell;

/// Where the verification key comes from, and the key once read.
pub struct PresenceKey {
    /// `None` for a key handed over whole ([`PresenceKey::fixed`]).
    path: Option<PathBuf>,
    key: OnceCell<Vec<u8>>,
}

impl PresenceKey {
    /// A key given directly — tests, and any wiring that already holds
    /// the bytes.
    pub fn fixed(key: Vec<u8>) -> Self {
        Self {
            path: None,
            key: OnceCell::new_with(Some(key)),
        }
    }

    /// The gateway's key file at `path`: read now when it holds a key,
    /// otherwise on first use (module doc). Silent when it is absent —
    /// that is the quickstart race, and [`PresenceKey::get`] says why
    /// at the first claim it cannot verify.
    pub async fn from_file(path: PathBuf) -> Self {
        let at_start = tokio::fs::read_to_string(&path)
            .await
            .ok()
            .and_then(|text| boss_core::presence::parse_session_key(&text).ok());
        Self {
            path: Some(path),
            key: OnceCell::new_with(at_start),
        }
    }

    /// The gateway's key file where the gateway itself looks for it
    /// (`BOSS_SESSION_KEY`, else its default path).
    pub async fn from_env() -> Self {
        Self::from_file(boss_core::presence::session_key_path()).await
    }

    /// Whether the key file now holds a DIFFERENT key than the one this
    /// process holds — a Secret replaced without a roll. Warns when it
    /// does; changes nothing, because the gateway still signs with the
    /// key it read at start (module doc).
    pub async fn changed_on_disk(&self) -> bool {
        let (Some(path), Some(held)) = (self.path.as_ref(), self.key.get()) else {
            return false;
        };
        let Ok(text) = tokio::fs::read_to_string(path).await else {
            return false;
        };
        let changed = boss_core::presence::parse_session_key(&text).is_ok_and(|now| &now != held);
        if changed {
            tracing::warn!(
                path = %path.display(),
                "presence: the gateway's key file holds a different key than this process \
                 read — the Secret was replaced without a roll of the boss Deployment. \
                 Presence claims are still refused; roll the Deployment so the gateway and \
                 this API read the same key"
            );
        }
        changed
    }

    /// The key, or `None` when there is none to be had — in which case
    /// nothing verifies. Says why on the log each time it cannot read
    /// one, because a presence refusal with a silent cause is the
    /// failure this whole change exists to remove.
    pub async fn get(&self) -> Option<&[u8]> {
        if let Some(key) = self.key.get() {
            return Some(key);
        }
        let path = self.path.as_ref()?;
        let text = match tokio::fs::read_to_string(path).await {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "presence: cannot read the gateway's key file; every presence claim \
                     is refused until it can be read"
                );
                return None;
            }
        };
        match boss_core::presence::parse_session_key(&text) {
            Ok(key) => {
                // A concurrent first read may have won; either way the
                // cell now holds the file's key.
                let _ = self.key.set(key);
                self.key.get().map(Vec::as_slice)
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "presence: the gateway's key file holds no usable key; every presence \
                     claim is refused"
                );
                None
            }
        }
    }
}

/// The key to judge `headers` with: looked up only when a presence
/// claim is actually present, so an ordinary write never touches the
/// file.
pub(super) async fn key_for<'a>(
    key: Option<&'a PresenceKey>,
    headers: &axum::http::HeaderMap,
) -> Option<&'a [u8]> {
    if !headers.contains_key(boss_core::presence::HEADER) {
        return None;
    }
    key?.get().await
}

/// What a refused claim says when the key file changed under this process.
pub(super) const KEY_CHANGED_DETAIL: &str = "An x-boss-presence header was presented and did \
     not verify, and the gateway's key file no longer holds the key this service read at \
     start: the Secret was replaced without a roll of the boss Deployment. The gateway still \
     signs with the key it read at start, so the claim stays refused; rolling the Deployment \
     makes both read the new key (design c22f36c6).";

/// `assured`, unchanged — unless its claim did not verify AND the key file
/// changed under this process, when its refusal says so (module doc).
pub(super) async fn name_a_changed_key(
    key: Option<&PresenceKey>,
    assured: super::steps::Assured,
) -> super::steps::Assured {
    match key {
        Some(key) if assured.unverified && key.changed_on_disk().await => super::steps::Assured {
            detail: KEY_CHANGED_DETAIL,
            ..assured
        },
        _ => assured,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_fixed_key_is_the_key() {
        let k = PresenceKey::fixed(vec![7; 32]);
        assert_eq!(k.get().await, Some(&[7u8; 32][..]));
    }

    /// The quickstart race: the jobs API asks before the gateway has
    /// written the file. It refuses then, and verifies once the file
    /// exists — without a restart.
    #[tokio::test]
    async fn a_key_file_written_after_start_is_picked_up() {
        let dir = boss_testing::scratch_dir("presence-key");
        let path = dir.join("session.key");
        let k = PresenceKey::from_file(path.clone()).await;
        assert_eq!(k.get().await, None, "no file yet: nothing verifies");

        std::fs::write(&path, "ab".repeat(32)).unwrap();
        assert_eq!(k.get().await, Some(&[0xab; 32][..]));
    }

    /// READ AT START WHEN IT IS THERE (design c22f36c6 item 6). The
    /// gateway reads the key once, at start; a jobs API that read it at
    /// its first presence claim held whatever the file said THEN, so a
    /// Secret replaced in between left the two signing and checking with
    /// different keys. Read at construction, the copy is the one this
    /// pod start saw — pinned here by removing the file after it.
    #[tokio::test]
    async fn a_key_file_present_at_start_is_read_at_start() {
        let dir = boss_testing::scratch_dir("presence-key-at-start");
        let path = dir.join("session.key");
        std::fs::write(&path, "cd".repeat(32)).unwrap();
        let k = PresenceKey::from_file(path.clone()).await;
        std::fs::remove_file(&path).unwrap();
        assert_eq!(k.get().await, Some(&[0xcd; 32][..]));
    }

    #[tokio::test]
    async fn a_malformed_key_file_verifies_nothing() {
        let dir = boss_testing::scratch_dir("presence-key-bad");
        let path = dir.join("session.key");
        std::fs::write(&path, "not-hex").unwrap();
        assert_eq!(PresenceKey::from_file(path).await.get().await, None);
    }

    /// A Secret replaced without a roll (design c22f36c6 item 6): the
    /// change is NAMED, and the key this process verifies with does not
    /// move, because the gateway still signs with the one it read at start.
    #[tokio::test]
    async fn a_key_changed_without_a_roll_is_named_and_not_adopted() {
        let dir = boss_testing::scratch_dir("presence-key-rotated");
        let path = dir.join("session.key");
        std::fs::write(&path, "ab".repeat(32)).unwrap();
        let k = PresenceKey::from_file(path.clone()).await;
        assert!(!k.changed_on_disk().await, "same bytes: nothing changed");

        std::fs::write(&path, "ef".repeat(32)).unwrap();
        assert!(k.changed_on_disk().await);
        assert_eq!(k.get().await, Some(&[0xab; 32][..]), "still the start key");
        // A fixed key has no file to change.
        assert!(!PresenceKey::fixed(vec![1; 32]).changed_on_disk().await);
    }
}
