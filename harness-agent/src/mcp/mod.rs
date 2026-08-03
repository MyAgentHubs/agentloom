pub mod client;
pub mod config;
pub mod host;
pub mod tool;

pub use host::{connect, McpHost};
pub use tool::{McpResourceListTool, McpResourceReadTool, McpToolProxy};
