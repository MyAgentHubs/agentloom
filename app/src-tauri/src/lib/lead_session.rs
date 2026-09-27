#[path = "lead_session/gate.rs"]
mod gate;
#[path = "lead_session/lead_ctx.rs"]
mod lead_ctx;
#[path = "lead_session/runner_thread.rs"]
mod runner_thread;
#[path = "lead_session/stream_closeout.rs"]
mod stream_closeout;
#[path = "lead_session/tool_registry.rs"]
mod tool_registry;

pub(crate) use gate::{reserve_lead_slot, resolve_lead_credentials};
pub(crate) use lead_ctx::{build_lead_ctx, load_member_pool, LeadRunFlags};
pub(crate) use runner_thread::{run_lead_runner, LeadRunnerCtx};
pub(crate) use stream_closeout::{
    decide_lead_terminals, persist_lead_closeout, pump_lead_stdout, wait_lead_exit, LeadDelivery,
};
pub(crate) use tool_registry::build_lead_tool_registry;
