// The parsing `build.rs` uses for the build commit (BRO-3842). `build.rs` `include!`s this file;
// `main.rs` compiles it only under `cfg(test)`, so these rules have tests (a build script itself
// runs only inside a cargo build). Plain comments, not `//!`: an included file's inner doc comment
// would sit mid-file in `build.rs`.

/// The commit's top-level entries, from `git ls-tree -z --name-only HEAD` exactly as git printed
/// them. `None`, which makes the build `-dirty`, when there are none, or when a name cannot be
/// handled exactly: not UTF-8 (it could not be passed on to `git status` unchanged), or holding a
/// control character such as a newline, which would end a `cargo:rerun-if-changed` line, so Cargo
/// could not watch it (P20 rounds 7 and 8).
pub(crate) fn top_entries(raw: &[u8]) -> Option<Vec<String>> {
    let text = std::str::from_utf8(raw).ok()?;
    let entries: Vec<String> = text.split('\0').filter(|e| !e.is_empty()).map(str::to_string).collect();
    if entries.is_empty() || entries.iter().any(|e| e.chars().any(char::is_control)) {
        return None;
    }
    Some(entries)
}

#[cfg(test)]
mod tests {
    use super::top_entries;

    #[test]
    fn names_are_split_on_nul_and_kept_exactly() {
        let raw = b" leading space\0Cargo.toml\0crates\0caf\xc3\xa9\0";
        assert_eq!(top_entries(raw).unwrap(), [" leading space", "Cargo.toml", "crates", "caf\u{e9}"]);
    }

    #[test]
    fn a_name_that_is_not_utf8_lists_nothing() {
        assert_eq!(top_entries(b"crates\0\xffbin\0"), None);
    }

    #[test]
    fn a_name_with_a_control_character_lists_nothing() {
        assert_eq!(top_entries(b"crates\0two\nlines\0"), None);
        assert_eq!(top_entries(b"crates\0tab\there\0"), None);
    }

    #[test]
    fn no_entry_lists_nothing() {
        assert_eq!(top_entries(b""), None);
        assert_eq!(top_entries(b"\0"), None);
    }
}
