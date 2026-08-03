use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::control::ControlSource;
#[cfg(test)]
use crate::error::HarnessError;
use crate::error::Result;
use crate::events::{EventRecorder, OutputMode};
use crate::goal::GoalState;
use crate::guardrails::Guardrails;
use crate::journal::{load_conversation, save_conversation, RunPaths, SavedConversation};
#[cfg(test)]
use crate::mcp::config::McpServerConfig;
use crate::plan::write_audit::TaskScope;
use crate::provider::pairing::repair_tool_pairing;
#[cfg(test)]
use crate::provider::ProviderCapabilities;
use crate::provider::{ChatMessage, ProviderClient};
use crate::shell::PermissionPolicy;
#[cfg(test)]
use crate::tools::ToolRegistry;

mod completion;
mod control;
mod entry;
mod evidence_gate;
mod persistence;
mod probe_runner;
mod progress_probe;
mod prompt;
mod run_loop;
mod signals;
mod tool_catalog;
mod tool_gate;
mod types;
mod verify_reflex;

pub(crate) use self::completion::*;
pub use self::control::*;
pub use self::entry::*;
pub use self::evidence_gate::*;
pub(crate) use self::persistence::*;
pub use self::probe_runner::*;
pub(crate) use self::progress_probe::*;
pub(crate) use self::prompt::*;
pub(crate) use self::run_loop::*;
pub(crate) use self::signals::*;
pub use self::tool_catalog::*;
pub use self::tool_gate::*;
pub use self::types::*;
pub(crate) use self::verify_reflex::*;

#[cfg(test)]
mod control_input_tests;
#[cfg(test)]
mod tests;
