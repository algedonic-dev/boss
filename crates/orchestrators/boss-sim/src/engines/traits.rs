//! `SimEngine` — the trait every pluggable sim engine implements.
//! The day-loop calls `step` once per simulated day (Periodic →
//! HumanWorker → Counterparty).

use crate::engines::DayContext;

pub trait SimEngine {
    /// Stable name for tracing + bus event provenance. Engines that
    /// host multiple specs (e.g. CounterpartyEngine) embed the spec
    /// name in their bus events as `<engine-name>:<spec-name>`.
    fn name(&self) -> &str;

    /// Advance one simulated day. Engines may read events emitted by
    /// upstream engines via `ctx.bus`, mutate `ctx.state`, draw from
    /// `ctx.rng`, and emit through `ctx.output` and `ctx.bus`.
    fn step(&mut self, ctx: &mut DayContext<'_>) -> anyhow::Result<()>;
}
