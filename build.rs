use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use syn::{Item, Meta};

/// Run `git` with the supplied arguments and return its trimmed stdout on
/// success. Returns `None` if git is unavailable or the command fails (for
/// example when building from a source tree that is not a git checkout).
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Build the version string embedded into the binaries:
///
/// * the output of `git describe --dirty` (with a leading `v` stripped, e.g.
///   `0.1-117-gef85e2b-dirty`), and
/// * the date and time of that commit (ISO 8601, e.g. `2026-09-18T17:28:34+01:00`).
///
/// `--always` is added so that describe still produces a commit hash when no
/// tags are present; it does not change the output when tags exist.
///
/// Falls back to the package version when git metadata is unavailable.
fn version() -> String {
    let Some(describe) = git(&["describe", "--dirty", "--always"]).filter(|s| !s.is_empty()) else {
        return env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_string());
    };
    let describe = describe.strip_prefix('v').unwrap_or(&describe);

    let date = git(&["log", "-1", "--format=%cI"]).unwrap_or_else(|| "unknown".to_string());

    format!("{describe} ({date})")
}

fn main() {
    // Re-run whenever the working tree or the checked-out commit changes so
    // that the embedded version (including the `-dirty` suffix) stays current.
    for path in [
        "src",
        "Cargo.toml",
        "build.rs",
        ".git/HEAD",
        ".git/index",
        ".git/packed-refs",
        ".git/refs/heads",
        ".git/refs/tags",
    ] {
        if Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    println!("cargo:rustc-env=DUTLIB_GIT_VERSION={}", version());

    generate_doc_lookup();
}

/// A source file scanned for rustdoc'd free functions by
/// [`generate_doc_lookup`].
struct DocSource {
    /// Path relative to the project root (i.e. `CARGO_MANIFEST_DIR`).
    path: &'static str,
    /// Module namespace used to qualify the symbols found in `path`. When
    /// `None` the namespace is derived from the path relative to `src/`
    /// (e.g. `src/tests.rs` lives in namespace `tests`).
    namespace: Option<&'static str>,
}

/// The source files scanned by [`generate_doc_lookup`]. To start indexing an
/// additional file, add an entry here.
const DOC_SOURCES: &[DocSource] = &[
    DocSource {
        path: "src/tests.rs",
        namespace: None,
    },
    DocSource {
        path: "src/tsn_tests.rs",
        namespace: None,
    },
];

/// A rustdoc'd free function found by [`generate_doc_lookup`].
struct DocEntry {
    namespace: String,
    name: String,
    path: String,
    doc: String,
}

/// Extract the text of a rustdoc comment (the `///` lines) from a list of
/// attributes. Returns `None` for undocumented items.
fn doc_comment(attrs: &[syn::Attribute]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for attr in attrs {
        // Both `/// text` and `#[doc = "text"]` parse as outer `doc`
        // attributes. Inner attributes (`//!`) can only appear on modules and
        // files, which are never passed here.
        if !matches!(attr.style, syn::AttrStyle::Outer) || !attr.path().is_ident("doc") {
            continue;
        }
        let Meta::NameValue(meta) = &attr.meta else {
            continue;
        };
        let syn::Expr::Lit(expr) = &meta.value else {
            continue;
        };
        let syn::Lit::Str(lit) = &expr.lit else {
            continue;
        };
        for line in lit.value().lines() {
            // `/// text` is stored as `" text"`; strip the single leading
            // space the same way rustdoc does.
            let line = line.strip_prefix(' ').unwrap_or(line);
            lines.push(line.trim_end().to_owned());
        }
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// Whether an attribute is `#[cfg(test)]`.
fn is_cfg_test(attr: &syn::Attribute) -> bool {
    if !attr.path().is_ident("cfg") {
        return false;
    }
    match &attr.meta {
        Meta::List(list) => list.tokens.to_string() == "test",
        _ => false,
    }
}

/// Recursively collect documented free functions from `items`, qualifying
/// their names with `namespace`. Methods (functions inside `impl` blocks) and
/// functions nested inside other functions are not free functions and are
/// skipped.
fn collect_entries(items: &[Item], namespace: &str, entries: &mut Vec<DocEntry>) {
    for item in items {
        match item {
            Item::Fn(func) => {
                if let Some(doc) = doc_comment(&func.attrs) {
                    let name = func.sig.ident.to_string();
                    let path = if namespace.is_empty() {
                        name.clone()
                    } else {
                        format!("{namespace}::{name}")
                    };
                    entries.push(DocEntry {
                        namespace: namespace.to_owned(),
                        name,
                        path,
                        doc,
                    });
                }
            }
            Item::Mod(module) => {
                // Skip unit test modules and `mod foo;` declarations (their
                // contents live in another file).
                if module.attrs.iter().any(is_cfg_test) {
                    continue;
                }
                if let Some((_, items)) = &module.content {
                    let nested_namespace = if namespace.is_empty() {
                        module.ident.to_string()
                    } else {
                        format!("{namespace}::{}", module.ident)
                    };
                    collect_entries(items, &nested_namespace, entries);
                }
            }
            _ => {}
        }
    }
}

/// Derive the module namespace for a source file from its path relative to
/// `src/`: `src/tests.rs` lives in namespace `tests`, `src/a/b.rs` in
/// namespace `a::b`.
fn derive_namespace(path: &str) -> String {
    let relative = Path::new(path)
        .strip_prefix("src")
        .unwrap_or(Path::new(path));
    let mut parts: Vec<String> = relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();

    // `src/foo/mod.rs` lives in namespace `foo`; a crate root file (`lib.rs`)
    // lives in the empty namespace.
    match parts.last() {
        Some(last) if last == "mod.rs" || last == "lib.rs" => {
            parts.pop();
        }
        _ => {}
    }
    if let Some(last) = parts.last_mut()
        && let Some(stem) = last.strip_suffix(".rs")
    {
        *last = stem.to_owned();
    }
    parts.join("::")
}

/// Generate `doc_lookup.rs` in `OUT_DIR`: a table of every rustdoc'd free
/// function found in [`DOC_SOURCES`] plus a `lookup()` helper that finds a
/// doc comment by bare symbol name or by namespace-qualified path.
fn generate_doc_lookup() {
    let mut entries: Vec<DocEntry> = Vec::new();

    for source in DOC_SOURCES {
        println!("cargo:rerun-if-changed={}", source.path);

        let text = fs::read_to_string(source.path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", source.path));
        let ast = syn::parse_file(&text)
            .unwrap_or_else(|err| panic!("failed to parse {}: {err}", source.path));

        let namespace = match source.namespace {
            Some(namespace) => namespace.to_owned(),
            None => derive_namespace(source.path),
        };
        collect_entries(&ast.items, &namespace, &mut entries);
    }

    // Sort by qualified path so that the generated file is deterministic.
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR not set"));
    fs::write(out_dir.join("doc_lookup.rs"), render(&entries))
        .expect("failed to write doc_lookup.rs");
}

/// Render the source of the generated lookup module.
fn render(entries: &[DocEntry]) -> String {
    let mut out = String::new();

    out.push_str("// Generated by build.rs — do not edit.\n");
    out.push_str("//\n");
    out.push_str("// Rustdoc'd free functions extracted from:\n");
    for source in DOC_SOURCES {
        out.push_str(&format!("//   {}\n", source.path));
    }

    out.push_str(
        "
/// A documented free function extracted at build time by `build.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocEntry {
    /// Bare function name, e.g. `ping`.
    pub name: &'static str,
    /// Module namespace, e.g. `tests`. Empty when the function is defined at
    /// the root of a scanned file.
    pub namespace: &'static str,
    /// Fully qualified path, e.g. `tests::ping`.
    pub path: &'static str,
    /// The rustdoc comment with the `///` markers stripped.
    pub doc: &'static str,
}

/// Every documented free function found in the scanned files, sorted by
/// [`DocEntry::path`].
pub static ENTRIES: &[DocEntry] = &[
",
    );

    for entry in entries {
        out.push_str(&format!(
            "    DocEntry {{\n        name: {},\n        namespace: {},\n        path: {},\n        doc: {},\n    }},\n",
            string_literal(&entry.name),
            string_literal(&entry.namespace),
            string_literal(&entry.path),
            string_literal(&entry.doc),
        ));
    }
    out.push_str("];\n");

    out.push_str(
        "
/// Look up the rustdoc comment for a symbol.
///
/// `symbol` may be either a bare function name (`ping`) or a
/// namespace-qualified path (`tests::ping`). A bare name only resolves when
/// it is unambiguous across all the scanned files; if it matches several
/// entries `None` is returned and [`ENTRIES`] can be used to list the
/// candidates.
pub fn lookup(symbol: &str) -> Option<&'static str> {
    let (namespace, name) = match symbol.rsplit_once(\"::\") {
        Some((namespace, name)) => (Some(namespace), name),
        None => (None, symbol),
    };

    let mut candidates = ENTRIES.iter().filter(|entry| {
        entry.name == name
            && match namespace {
                Some(namespace) => entry.namespace == namespace,
                None => true,
            }
    });

    let first = candidates.next()?;
    if namespace.is_none() && candidates.next().is_some() {
        // A bare name matching several entries cannot be resolved.
        return None;
    }
    Some(first.doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_resolves_qualified_paths() {
        for entry in ENTRIES {
            assert_eq!(lookup(entry.path), Some(entry.doc), \"{}\", entry.path);
        }
    }

    #[test]
    fn lookup_resolves_unambiguous_bare_names() {
        for entry in ENTRIES {
            let occurrences = ENTRIES.iter().filter(|other| other.name == entry.name).count();
            if occurrences == 1 {
                assert_eq!(lookup(entry.name), Some(entry.doc), \"{}\", entry.name);
            }
        }
    }

    #[test]
    fn lookup_rejects_unknown_symbols() {
        assert_eq!(lookup(\"definitely_not_a_function\"), None);
        assert_eq!(
            lookup(\"definitely_not_a_namespace::definitely_not_a_function\"),
            None
        );
    }
}
",
    );

    out
}

/// Escape `text` as a Rust string literal.
fn string_literal(text: &str) -> String {
    format!("{text:?}")
}
