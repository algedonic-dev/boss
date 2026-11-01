//! A credential-broker client the dispatcher could not build is said, as
//! an error, at boot.
//!
//! Backlog 3107bfb1 (review a2172ed3 of car 01872c8e). The broker's forge
//! admin client and its in-cluster k8s client became fallible in 6162aff2
//! (each is built with every proxy off), and the binary answered a build
//! failure by registering `Unconfigured` in its place — correctly, since
//! an unregistered handler aborts every co-fired rule — and logging
//! nothing. The dispatcher booted green, and the first anyone heard of a
//! broker that could not rotate was a rotation dead-lettering on its
//! packet. `Unconfigured::at_boot` is the one way the binary takes that
//! fallback, and it logs an ERROR naming the reason as it does.
//!
//! **This file holds exactly one test, and that is the point.** It
//! asserts what was logged, and a captured log is deterministic only when
//! the capturing test is alone in its process with the subscriber
//! installed globally before the first event
//! (`boss-testing/tests/a_log_capturing_test_owns_its_process.rs`).

use boss_dispatcher_handlers::handlers::credential_issuer::{SecretStore, Unconfigured};
use std::io::Write;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn a_client_the_broker_could_not_build_is_an_error_at_boot() {
    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("this binary holds one test, so nothing else has claimed the subscriber");

    const WHY: &str = "credential broker has no in-cluster k8s credential: read SA token: gone";
    let fallback = Unconfigured::at_boot(WHY.to_string());

    let text = log.text();
    let line = text
        .lines()
        .find(|l| l.contains(WHY))
        .unwrap_or_else(|| panic!("the fallback was not logged with its reason:\n{text}"));
    assert!(
        line.contains("ERROR"),
        "the fallback was logged below error: {line}"
    );

    // And what it falls back to still refuses with the same reason, so a
    // firing rule dead-letters naming it, as before.
    let refused = fallback
        .read_key("boss", "boss-dev-forge", "token")
        .await
        .expect_err("an Unconfigured store refuses");
    assert_eq!(refused, WHY);
}
