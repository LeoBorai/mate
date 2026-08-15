//! Filesystem tools for the agent: `read_file`, `list_dir`, `find_files`. Every path is
//! canonicalized and jailed under the session's workspace root (via
//! `mate_tool_api::ToolCtx::resolve`), with a denylist for sensitive files (`.env`,
//! `*.pem`, `id_rsa*`, `.git/`).

mod find_files;
mod list_dir;
mod read_file;

pub use find_files::{FindFiles, FindFilesArgs};
pub use list_dir::{ListDir, ListDirArgs};
pub use read_file::{ReadFile, ReadFileArgs};
