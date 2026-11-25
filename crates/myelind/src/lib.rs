//! Myelin's procedural-memory engine, now hosted inside the NexusContext
//! workspace. The binary in `main.rs` is kept as a compatibility shim; the
//! authoritative MCP surface is folded into `nexusd`'s tool catalog.

pub mod control;
pub mod mcp;
pub mod tools;
