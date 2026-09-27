//! One body of assertions, run against every adapter of a port —
//! reliability mechanism C of design 3036296f, "adapters agree".
//!
//! WHY IT EXISTS (backlog 67c15125, 2026-09-27). A port with two
//! adapters states its contract twice, once per adapter, and nothing
//! but a test holds the two statements to each other (CLAUDE.md §9a).
//! The Postgres jobs adapter ignored `JobFilter::priority` while the
//! in-memory adapter honoured it (backlog 74569e94), so a listing
//! filtered by priority answered every packet — and every test of the
//! filter ran against the in-memory adapter, which was right. Three
//! earlier files already ran one generic body against both adapters
//! (`the_adapters_agree_on_*_pg.rs` in boss-jobs), each wiring its own
//! pair of `#[tokio::test]`s by hand. This macro is that wiring, written
//! once, so a port's suite is its cases and nothing else.
//!
//! # Shape
//!
//! A CASE is an `async fn` generic over the port, taking the adapter
//! and the adapter's name (for the assertion messages):
//!
//! ```ignore
//! async fn kind<R: JobsRepository>(repo: &R, adapter: &str) { … }
//! ```
//!
//! An ADAPTER is a name and an expression evaluating to `(store, held)`
//! inside an async test body — `held` is whatever must outlive the
//! case (a `TestDb`), `()` when nothing does:
//!
//! ```ignore
//! boss_testing::adapters_agree! {
//!     adapters {
//!         in_memory => (InMemoryJobs::new(), ()),
//!         postgres => {
//!             let db = boss_testing::TestDb::new().await;
//!             (boss_jobs::PgJobs::new(db.pool.clone()), db)
//!         },
//!     }
//!     cases { kind, status, priority }
//! }
//! ```
//!
//! Every (case, adapter) pair becomes its own test, `<case>::<adapter>`
//! — so a red names the case AND the adapter that disagreed, rather
//! than one test per adapter stopping at the first bad case and hiding
//! the rest. Each pair gets a fresh store, so no case can pass on
//! another's seed. The generated module takes `use super::*`, so the
//! adapter expressions resolve whatever the calling file imported.

/// Run each named case against each named adapter, one test per pair.
/// See `boss_testing::adapter_suite` for the shape and the reason.
#[macro_export]
macro_rules! adapters_agree {
    (
        adapters $adapters:tt
        cases { $($case:ident),+ $(,)? }
    ) => {
        // The adapter block rides as ONE token tree: `macro_rules!`
        // cannot nest a repetition of adapters inside the repetition of
        // cases, but it can hand the whole block to each case.
        $( $crate::adapters_agree!(@case $case $adapters); )+
    };
    (@case $case:ident { $($adapter:ident => $make:expr),+ $(,)? }) => {
        // A module and a function may share a name: they live in the
        // type and value namespaces respectively, so `<case>::<adapter>`
        // is the test and the parent's fn `<case>` is the body it runs.
        // The body is called through the glob, not as `super::<case>`,
        // so the glob is always used — an adapter expression written in
        // full paths would otherwise leave it unused, and a bare allow
        // is refused (a-dead-code-allowance-needs-a-reason).
        mod $case {
            use super::*;
            $(
                #[tokio::test(flavor = "multi_thread")]
                async fn $adapter() {
                    let (store, _held) = $make;
                    $case(&store, stringify!($adapter)).await;
                }
            )+
        }
    };
}

#[cfg(test)]
mod tests {
    //! The macro, on a toy port with two adapters that disagree on
    //! nothing.

    trait Doubler {
        fn double(&self, n: u32) -> u32;
    }
    struct Adds;
    impl Doubler for Adds {
        fn double(&self, n: u32) -> u32 {
            n + n
        }
    }
    struct Shifts;
    impl Doubler for Shifts {
        fn double(&self, n: u32) -> u32 {
            n << 1
        }
    }

    async fn doubles<D: Doubler>(port: &D, adapter: &str) {
        assert_eq!(port.double(21), 42, "{adapter}");
        assert!(matches!(adapter, "adds" | "shifts"), "named {adapter}");
    }

    async fn doubles_zero<D: Doubler>(port: &D, adapter: &str) {
        assert_eq!(port.double(0), 0, "{adapter}");
    }

    crate::adapters_agree! {
        adapters {
            adds => (Adds, ()),
            // `held` is any value that must live until the case
            // returns, as a TestDb must.
            shifts => (Shifts, String::from("held")),
        }
        // Four tests: doubles::adds, doubles::shifts,
        // doubles_zero::adds, doubles_zero::shifts.
        cases { doubles, doubles_zero }
    }
}
