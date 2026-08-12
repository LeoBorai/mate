//! Filesystem tools for the agent: `read_file`, `list_dir`, `find_files`. Every
//! path is canonicalized and jailed under the session's workspace root, with a
//! denylist for sensitive files (`.env`, `*.pem`, `id_rsa*`, `.git/`).
