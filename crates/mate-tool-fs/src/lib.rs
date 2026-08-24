//! Filesystem tools for the agent: `read_file`, `list_dir`, `find_files`, `write_file`. Every
//! path is canonicalized and jailed under the session's workspace root (via
//! `mate_tool_api::ToolCtx::resolve`/`resolve_for_write`), with a denylist for sensitive files
//! (`.env`, `*.pem`, `id_rsa*`, `.git/`). `write_file` additionally gates every call through
//! `mate_tool_api::ToolCtx::approvals` (§7.4) — the other three are read-only and never ask.

mod find_files;
mod list_dir;
mod read_file;
mod write_file;

pub use find_files::{FindFiles, FindFilesArgs};
pub use list_dir::{ListDir, ListDirArgs};
pub use read_file::{ReadFile, ReadFileArgs};
pub use write_file::{WriteFile, WriteFileArgs};
