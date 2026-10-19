//! (inner attribute first: with nine command files the const scan exceeds
//! rustc's default const-eval step budget. The lint exists to catch runaway
//! const loops; this one terminates, it is just linear in the source bytes.)
#![allow(long_running_const_eval)]

//! Compile-time guard against unregistered Tauri commands.
//!
//! Three Level Editor features have shipped as dead code in this project
//! because a `#[tauri::command]` was written, compiled, and never added to the
//! `generate_handler!` list in `lib.rs`. Two of those command modules were not
//! even declared as `mod`, so `cargo check` was perfectly happy with them.
//!
//! This module reads the command sources and `lib.rs` back as text with
//! `include_str!` and asserts, **in const context**, that every
//! `#[tauri::command]` in one of the scanned command files is named somewhere
//! in `lib.rs`. A const assertion fails during `cargo check --lib`, which is the
//! gate this project actually runs, so a forgotten registration cannot reach a
//! commit.
//!
//! When the assertion fires it can only report a count -- const evaluation has
//! no formatting. Run `cargo test -p omnitool command_guard` for the list of
//! offending names.
//!
//! Known limits of a textual guard, stated so nobody trusts it too far:
//!   * it matches a bare `::<name>,` on any line of `lib.rs` that is not a `//`
//!     comment, so a command named in a `/* */` block or in a string would
//!     satisfy it. Line-commenting a registration is caught.
//!   * it only covers the files listed in [`SOURCES`] -- every
//!     `commands_level_*.rs` plus the arena and rivet modules the upstream
//!     merge added. `commands.rs` and `commands_model_inspector.rs` predate the
//!     guard and are not scanned.

/// The file the registrations must appear in.
const LIB_SRC: &str = include_str!("lib.rs");

/// Every command module the guard covers, with its file name for the message.
const SOURCES: &[(&str, &str)] = &[
    (
        "commands_level_editor.rs",
        include_str!("commands_level_editor.rs"),
    ),
    (
        "commands_level_entities.rs",
        include_str!("commands_level_entities.rs"),
    ),
    (
        "commands_level_nav.rs",
        include_str!("commands_level_nav.rs"),
    ),
    (
        "commands_level_graphs.rs",
        include_str!("commands_level_graphs.rs"),
    ),
    (
        "commands_level_lighting.rs",
        include_str!("commands_level_lighting.rs"),
    ),
    (
        "commands_level_prius.rs",
        include_str!("commands_level_prius.rs"),
    ),
    (
        "commands_level_script.rs",
        include_str!("commands_level_script.rs"),
    ),
    (
        "commands_level_world.rs",
        include_str!("commands_level_world.rs"),
    ),
    // The command modules the upstream merge brought in. They are not
    // `commands_level_*`, but they are the same shape of file and the same
    // mistake is available in them, so the guard covers them too.
    ("commands_arena.rs", include_str!("commands_arena.rs")),
    (
        "commands_arena_graph.rs",
        include_str!("commands_arena_graph.rs"),
    ),
    ("commands_rivet.rs", include_str!("commands_rivet.rs")),
];

const ATTR: &[u8] = b"#[tauri::command]";
const PUB_ASYNC_FN: &[u8] = b"pub async fn ";
const PUB_FN: &[u8] = b"pub fn ";

/// How far past the attribute we will look for the function it decorates.
/// Enough for the `#[allow(...)]` lines that sit between the two.
const ATTR_TO_FN_WINDOW: usize = 512;

const fn eq_at(hay: &[u8], at: usize, needle: &[u8]) -> bool {
    if at + needle.len() > hay.len() {
        return false;
    }
    let mut i = 0;
    while i < needle.len() {
        if hay[at + i] != needle[i] {
            return false;
        }
        i += 1;
    }
    true
}

const fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Does the line containing `at` start with `//`?
///
/// A commented-out registration must not satisfy the guard. Someone silencing a
/// line to bisect a build is exactly as broken as never writing it.
const fn line_is_commented(lib: &[u8], at: usize) -> bool {
    // Back up to the first byte of this line.
    let mut start = at;
    while start > 0 && lib[start - 1] != b'\n' {
        start -= 1;
    }
    // Skip indentation.
    let mut p = start;
    while p < lib.len() && (lib[p] == b' ' || lib[p] == b'\t') {
        p += 1;
    }
    eq_at(lib, p, b"//")
}

/// Is `hay[from..to]` named as `::<that>,` on a live (uncommented) line of `lib`?
const fn lib_names(lib: &[u8], hay: &[u8], from: usize, to: usize) -> bool {
    let n = to - from;
    if n == 0 {
        return true; // not a name we can check; do not cry wolf
    }
    let mut p = 0;
    while p + 2 + n + 1 <= lib.len() {
        if lib[p] == b':' && lib[p + 1] == b':' && lib[p + 2 + n] == b',' {
            let mut k = 0;
            let mut ok = true;
            while k < n {
                if lib[p + 2 + k] != hay[from + k] {
                    ok = false;
                    break;
                }
                k += 1;
            }
            if ok && !line_is_commented(lib, p) {
                return true;
            }
        }
        p += 1;
    }
    false
}

/// Number of `#[tauri::command]` functions in `src` that `lib` never names.
const fn unregistered(src: &str, lib: &str) -> usize {
    let s = src.as_bytes();
    let l = lib.as_bytes();
    let mut missing = 0usize;
    let mut i = 0usize;
    while i < s.len() {
        if !eq_at(s, i, ATTR) {
            i += 1;
            continue;
        }
        let mut j = i + ATTR.len();
        let stop = if j + ATTR_TO_FN_WINDOW < s.len() {
            j + ATTR_TO_FN_WINDOW
        } else {
            s.len()
        };
        let mut name_start = 0usize;
        let mut found = false;
        while j < stop {
            if eq_at(s, j, PUB_ASYNC_FN) {
                name_start = j + PUB_ASYNC_FN.len();
                found = true;
                break;
            }
            if eq_at(s, j, PUB_FN) {
                name_start = j + PUB_FN.len();
                found = true;
                break;
            }
            j += 1;
        }
        if found {
            let mut e = name_start;
            while e < s.len() && is_ident_byte(s[e]) {
                e += 1;
            }
            if !lib_names(l, s, name_start, e) {
                missing += 1;
            }
            i = e;
        } else {
            // An attribute with no function after it: the file no longer looks
            // the way this guard assumes. Count it so somebody looks.
            missing += 1;
            i += ATTR.len();
        }
    }
    missing
}

const fn total_unregistered() -> usize {
    let mut n = 0usize;
    let mut i = 0usize;
    while i < SOURCES.len() {
        n += unregistered(SOURCES[i].1, LIB_SRC);
        i += 1;
    }
    n
}

const UNREGISTERED: usize = total_unregistered();

/// Skipped under `cfg(test)` on purpose: a failing const assertion aborts
/// compilation, which would stop the very test that names the offenders from
/// ever running. `cargo check` / `cargo build` keep the hard failure.
#[cfg(not(test))]
const _: () = assert!(
    UNREGISTERED == 0,
    "a #[tauri::command] in src/commands_level_*.rs is missing from the \
     generate_handler! list in src/lib.rs, so the frontend cannot call it. \
     Run `cargo test -p omnitool command_guard` to see which ones."
);

/// The same scan at runtime, so the test can name the offenders.
/// Returns `(file, command)` pairs that `lib.rs` never mentions.
pub fn unregistered_commands() -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for (file, src) in SOURCES {
        for chunk in src.split(ATTR_STR).skip(1) {
            let Some(name) = fn_name(chunk) else {
                out.push((*file, "<attribute with no fn after it>".to_string()));
                continue;
            };
            let needle = format!("::{name},");
            let registered = LIB_SRC
                .lines()
                .any(|l| !l.trim_start().starts_with("//") && l.contains(&needle));
            if !registered {
                out.push((*file, name));
            }
        }
    }
    out
}

const ATTR_STR: &str = "#[tauri::command]";

fn fn_name(chunk: &str) -> Option<String> {
    let window = &chunk[..chunk.len().min(ATTR_TO_FN_WINDOW)];
    let rest = match window.find("pub async fn ") {
        Some(p) => &window[p + "pub async fn ".len()..],
        None => {
            let p = window.find("pub fn ")?;
            &window[p + "pub fn ".len()..]
        }
    };
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Every command the guard knows about, registered or not. Useful for a
/// harness that wants to print the surface area.
pub fn all_commands() -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for (file, src) in SOURCES {
        for chunk in src.split(ATTR_STR).skip(1) {
            if let Some(name) = fn_name(chunk) {
                out.push((*file, name));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_level_command_is_registered() {
        let missing = unregistered_commands();
        assert!(
            missing.is_empty(),
            "these #[tauri::command] functions are not in generate_handler! in \
             src/lib.rs, so the frontend cannot invoke them:\n{}",
            missing
                .iter()
                .map(|(f, c)| format!("  {f}: {c}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn the_guard_actually_finds_commands() {
        // A guard that parses nothing would pass the test above vacuously.
        let all = all_commands();
        assert!(
            all.len() >= 30,
            "the scan found only {} commands across {} files; the parse has \
             probably stopped matching the source layout",
            all.len(),
            SOURCES.len()
        );
    }
}
