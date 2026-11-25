//! Myelin's procedural-memory engine, now hosted inside the NexusContext
//! workspace. The binary in `main.rs` is kept as a compatibility shim; the
//! authoritative MCP surface is folded into `nexusd`'s tool catalog.

// The control API is a Unix-domain socket with no cross-platform transport,
// same boundary as `nexusd serve` (issue #16). MCP stdio works everywhere.
#[cfg(unix)]
pub mod control;
pub mod mcp;
pub mod tools;
