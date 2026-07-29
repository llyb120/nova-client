//! Agent runtime, ported from `pi-agent-core` (`agent.js`, `agent-loop.js`).
//!
//! Milestone M3 builds this up in slices: message construction (done), the
//! `runLoop` state machine, tool-execution scheduling, and the `Agent`
//! lifecycle wrapper. The LLM boundary is abstracted so parity tests can drive
//! it with a deterministic mock stream.

pub mod messages;
pub mod run_loop;

pub use messages::{create_error_tool_result, create_tool_result_message};
pub use run_loop::{run_agent_loop, LoopConfig, LoopContext};
