pub mod http;
pub mod params;
pub mod server;

// Re-exports for later tasks (lib wiring / settings UI).
#[allow(unused_imports)]
pub use http::build_mcp_router;
#[allow(unused_imports)]
pub use server::{ChatMemoryMcp, MCP_PORT, MCP_URL};
