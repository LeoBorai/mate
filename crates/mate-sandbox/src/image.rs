//! The sandbox image shipped inside `mate`: its Dockerfile and `tmux.conf` are embedded in the
//! binary, tagged by a hash of their contents, and sent to the daemon as an in-memory tar build
//! context. A changed file gives a new tag, so the image is rebuilt only when it must be.
//!
//! API used, read from source (downloaded from crates.io, not `~/.cargo/registry/src/`, which
//! has neither crate):
//!
//! - `tar` 0.4.46, `tar-0.4.46/src/builder.rs`: `Builder::new(W: Write)`,
//!   `Builder::append_data(&mut Header, impl AsRef<Path>, impl Read)` (sets the path, then the
//!   checksum), `Builder::into_inner() -> io::Result<W>` (finishes the archive). From
//!   `src/header.rs`: `Header::new_gnu()`, `set_size(u64)`, `set_mode(u32)`.
//! - `bollard` 0.21.1, `src/image.rs`: `Docker::build_image(impl Into<BuildImageOptions>,
//!   Option<HashMap<String, DockerCredentials>>, Option<BodyType>)` returns a `Stream` of
//!   `Result<BuildInfo, Error>`. `bollard-stubs` 1.53.1-rc.29.3.1: `BuildInfo { stream,
//!   error_detail: Option<ErrorDetail { message, .. }>, status, .. }` and
//!   `query_parameters::BuildImageOptionsBuilder` (`dockerfile`, `t`, `rm`, `build`).
//!   `src/docker.rs`: `body_full(Bytes) -> BodyType`.

use std::io;

use sha2::{Digest, Sha256};

/// The embedded Dockerfile.
pub const DOCKERFILE: &str = include_str!("../image/Dockerfile");
/// The embedded system tmux configuration.
pub const TMUX_CONF: &str = include_str!("../image/tmux.conf");

const TAG_PREFIX: &str = "mate-sandbox:";
const HASH_LEN: usize = 12;

/// `mate-sandbox:<hash12>` for the embedded files.
pub fn embedded_tag() -> String {
    tag_for(DOCKERFILE, TMUX_CONF)
}

/// The tag for the given file contents. Each input is length-prefixed so moving bytes from one
/// file to the other changes the tag.
pub fn tag_for(dockerfile: &str, tmux_conf: &str) -> String {
    let mut hasher = Sha256::new();
    for part in [dockerfile, tmux_conf] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{TAG_PREFIX}{}", &hex[..HASH_LEN])
}

/// Whether `reference` is the current embedded image's tag. Only that one is ever built; any
/// other reference (a configured override, or an older embedded tag) is pulled.
pub fn is_embedded_tag(reference: &str) -> bool {
    reference == embedded_tag()
}

/// The image to use: a configured override replaces the embedded one entirely.
pub fn default_image(configured: Option<&str>) -> String {
    configured.map_or_else(embedded_tag, str::to_owned)
}

/// The in-memory tar build context holding exactly `Dockerfile` and `tmux.conf`.
pub fn build_context() -> io::Result<Vec<u8>> {
    context_for(DOCKERFILE, TMUX_CONF)
}

fn context_for(dockerfile: &str, tmux_conf: &str) -> io::Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, content) in [("Dockerfile", dockerfile), ("tmux.conf", tmux_conf)] {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        builder.append_data(&mut header, name, content.as_bytes())?;
    }
    builder.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn the_tag_is_the_prefix_plus_twelve_hex_characters() {
        let tag = embedded_tag();
        let hash = tag.strip_prefix("mate-sandbox:").expect("tag has the mate-sandbox prefix");
        assert_eq!(hash.len(), 12, "the hash part is 12 characters");
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "the hash part is lowercase hex, got {hash}"
        );
    }

    #[test]
    fn the_tag_changes_when_either_file_changes() {
        let base = tag_for("FROM a", "set -g status off");
        assert_eq!(
            base,
            tag_for("FROM a", "set -g status off"),
            "the same content always gives the same tag"
        );
        assert_ne!(base, tag_for("FROM b", "set -g status off"), "a Dockerfile change retags");
        assert_ne!(base, tag_for("FROM a", "set -g status on"), "a tmux.conf change retags");
        assert_ne!(
            tag_for("ab", "c"),
            tag_for("a", "bc"),
            "moving bytes between the files retags"
        );
    }

    #[test]
    fn the_context_holds_exactly_the_two_embedded_files() {
        let bytes = build_context().unwrap();
        let mut archive = tar::Archive::new(bytes.as_slice());
        let mut found = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().to_string_lossy().into_owned();
            let mut content = String::new();
            entry.read_to_string(&mut content).unwrap();
            found.push((name, content));
        }
        assert_eq!(
            found,
            vec![
                ("Dockerfile".to_owned(), DOCKERFILE.to_owned()),
                ("tmux.conf".to_owned(), TMUX_CONF.to_owned()),
            ],
            "the context is the Dockerfile then tmux.conf, with their embedded content"
        );
    }

    #[test]
    fn the_default_image_is_the_embedded_tag_unless_overridden() {
        assert_eq!(default_image(None), embedded_tag(), "unset means the embedded image");
        assert_eq!(
            default_image(Some("example.com/x:1")),
            "example.com/x:1",
            "a configured image replaces it"
        );
    }

    #[test]
    fn only_the_current_embedded_tag_is_built() {
        assert!(is_embedded_tag(&embedded_tag()), "the embedded tag is built");
        assert!(!is_embedded_tag("mate-sandbox:000000000000"), "an old tag is not rebuilt");
        assert!(!is_embedded_tag("alpine:3"), "a registry image is pulled");
    }
}
