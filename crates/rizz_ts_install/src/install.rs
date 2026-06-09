//! Orchestrates the fetch → build → cache flow for a single grammar.
//!
//! All side effects (network via `git`, compilation via `tree-sitter build`,
//! filesystem writes) live here. The rest of the crate stays pure.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tracing::{debug, info, instrument, warn};

use crate::cache::{self, CachedGrammar};
use crate::manifest::Manifest;
use crate::{InstallError, library_filename};

/// Per-call overrides on top of the curated manifest entry. Every field is
/// optional; `(grammar-install 'rust)` with no extra args produces
/// `InstallOpts::default()`.
#[derive(Debug, Default, Clone)]
pub struct InstallOpts {
    pub repo: Option<String>,
    /// Use an on-disk checkout instead of cloning. When set, `repo`/`branch`/
    /// `rev` are ignored.
    pub path: Option<PathBuf>,
    pub branch: Option<String>,
    pub rev: Option<String>,
    pub subdir: Option<String>,
    pub extensions: Option<Vec<String>>,
    pub language: Option<String>,
    pub queries: Option<String>,
    pub force: bool,
}

/// Successful install result. The editor side hands `library` and `highlights`
/// to `TsRegistry::register` to wire the grammar in.
#[derive(Debug, Clone)]
pub struct InstalledGrammar {
    pub name: String,
    pub language: String,
    pub extensions: Vec<String>,
    pub library: PathBuf,
    pub highlights: PathBuf,
}

/// Resolved spec — the merge of the manifest entry and the per-call opts.
#[derive(Debug)]
struct Resolved {
    repo: Option<String>,
    path: Option<PathBuf>,
    branch: Option<String>,
    rev: Option<String>,
    subdir: Option<String>,
    extensions: Vec<String>,
    language: String,
    queries: Option<String>,
}

fn resolve(name: &str, opts: &InstallOpts, manifest: &Manifest) -> Result<Resolved, InstallError> {
    let entry = manifest.get(name);
    let repo = opts
        .repo
        .clone()
        .or_else(|| entry.map(|e| e.repo.clone()));
    let path = opts.path.clone();
    if repo.is_none() && path.is_none() {
        return Err(InstallError::UnknownGrammar {
            name: name.to_string(),
        });
    }
    let branch = opts
        .branch
        .clone()
        .or_else(|| entry.and_then(|e| e.branch.clone()));
    let rev = opts
        .rev
        .clone()
        .or_else(|| entry.and_then(|e| e.rev.clone()));
    let subdir = opts
        .subdir
        .clone()
        .or_else(|| entry.and_then(|e| e.subdir.clone()));
    let extensions = opts
        .extensions
        .clone()
        .or_else(|| entry.map(|e| e.extensions.clone()))
        .unwrap_or_default();
    let language = opts
        .language
        .clone()
        .or_else(|| entry.and_then(|e| e.language.clone()))
        .unwrap_or_else(|| name.to_string());
    let queries = opts
        .queries
        .clone()
        .or_else(|| entry.and_then(|e| e.queries.clone()));
    Ok(Resolved {
        repo,
        path,
        branch,
        rev,
        subdir,
        extensions,
        language,
        queries,
    })
}

fn stamp_for(resolved: &Resolved, head_sha: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(repo) = &resolved.repo {
        out.push_str("repo=");
        out.push_str(repo);
        out.push('\n');
    }
    if let Some(path) = &resolved.path {
        out.push_str("path=");
        out.push_str(&path.display().to_string());
        out.push('\n');
    }
    if let Some(rev) = &resolved.rev {
        out.push_str("rev=");
        out.push_str(rev);
        out.push('\n');
    }
    if let Some(sha) = head_sha {
        out.push_str("sha=");
        out.push_str(sha);
        out.push('\n');
    }
    out
}

/// Fetch (if needed), build, cache, and return paths to the installed
/// grammar. Idempotent: a matching cache stamp short-circuits the network.
#[instrument(skip(opts, manifest), fields(name = name))]
pub fn install(
    name: &str,
    opts: &InstallOpts,
    manifest: &Manifest,
) -> Result<InstalledGrammar, InstallError> {
    let resolved = resolve(name, opts, manifest)?;
    let cache_root = cache::cache_root();
    let grammar_dir = cache::grammar_dir(&cache_root, name);
    std::fs::create_dir_all(&grammar_dir).map_err(|source| InstallError::Io {
        path: grammar_dir.clone(),
        source,
    })?;

    // 1. Acquire source: either point at a user-supplied checkout, or sync
    //    a managed clone under <cache>/<name>/src.
    let (source_root, head_sha) = if let Some(path) = &resolved.path {
        if !path.exists() {
            return Err(InstallError::MissingSource { path: path.clone() });
        }
        (path.clone(), None)
    } else {
        let src = cache::source_dir(&cache_root, name);
        let sha = sync_clone(&src, &resolved, opts.force)?;
        (src, Some(sha))
    };

    // 2. Short-circuit if cache matches stamp and we're not forcing a rebuild.
    let want_stamp = stamp_for(&resolved, head_sha.as_deref());
    if !opts.force
        && let Some(cached) = CachedGrammar::read(&cache_root, name)
        && cached.stamp.as_deref() == Some(want_stamp.as_str())
    {
        debug!(name, "cache hit — skipping build");
        return Ok(InstalledGrammar {
            name: name.to_string(),
            language: resolved.language,
            extensions: resolved.extensions,
            library: cached.library,
            highlights: cached.highlights,
        });
    }

    // 3. Build the parser library. tree-sitter build runs from inside the
    //    grammar's source dir (`<subdir>` if vendored multi-grammar repo).
    let build_dir = match &resolved.subdir {
        Some(s) => source_root.join(s),
        None => source_root.clone(),
    };
    if !build_dir.exists() {
        return Err(InstallError::MissingSource {
            path: build_dir.clone(),
        });
    }
    let library = cache::library_path(&cache_root, name);
    info!(?build_dir, ?library, "building grammar");
    run_tree_sitter_build(&build_dir, &library)?;

    // 4. Copy highlights.scm into the cache. Default location is
    //    `<build_dir>/queries/highlights.scm`; `queries` opt overrides.
    let queries_rel = resolved
        .queries
        .clone()
        .unwrap_or_else(|| "queries/highlights.scm".to_string());
    let queries_src = if Path::new(&queries_rel).is_absolute() {
        PathBuf::from(&queries_rel)
    } else {
        build_dir.join(&queries_rel)
    };
    if !queries_src.exists() {
        return Err(InstallError::MissingHighlights { path: queries_src });
    }
    let highlights = cache::highlights_path(&cache_root, name);
    std::fs::copy(&queries_src, &highlights).map_err(|source| InstallError::Io {
        path: highlights.clone(),
        source,
    })?;

    // 5. Write the stamp so subsequent installs of the same spec are no-ops.
    let stamp = cache::stamp_path(&cache_root, name);
    std::fs::write(&stamp, &want_stamp).map_err(|source| InstallError::Io {
        path: stamp,
        source,
    })?;

    info!(name, "installed grammar");
    Ok(InstalledGrammar {
        name: name.to_string(),
        language: resolved.language,
        extensions: resolved.extensions,
        library,
        highlights,
    })
}

/// Pure cache lookup used by the auto-load-on-buffer-open hook. Returns
/// `None` when the cache is missing the parser library or highlights file,
/// or when the spec resolves to nothing useful.
pub fn try_load_cached(
    name: &str,
    opts: &InstallOpts,
    manifest: &Manifest,
) -> Option<InstalledGrammar> {
    let resolved = resolve(name, opts, manifest).ok()?;
    let root = cache::cache_root();
    let cached = CachedGrammar::read(&root, name)?;
    Some(InstalledGrammar {
        name: name.to_string(),
        language: resolved.language,
        extensions: resolved.extensions,
        library: cached.library,
        highlights: cached.highlights,
    })
}

// ----- shell-out helpers ------------------------------------------------

fn run_capture(cmd: &mut Command, tool: &'static str) -> Result<Output, InstallError> {
    cmd.output()
        .map_err(|source| InstallError::ToolNotFound { tool, source })
}

fn sync_clone(src: &Path, resolved: &Resolved, force: bool) -> Result<String, InstallError> {
    let repo = resolved
        .repo
        .as_deref()
        .expect("sync_clone called without a repo");
    if force && src.exists() {
        std::fs::remove_dir_all(src).map_err(|source| InstallError::Io {
            path: src.to_path_buf(),
            source,
        })?;
    }
    if !src.exists() {
        std::fs::create_dir_all(src.parent().unwrap()).map_err(|source| InstallError::Io {
            path: src.to_path_buf(),
            source,
        })?;
        let mut cmd = Command::new("git");
        cmd.arg("clone");
        if resolved.rev.is_none() {
            cmd.arg("--depth").arg("1");
        }
        if let Some(branch) = &resolved.branch {
            cmd.arg("--branch").arg(branch);
        }
        cmd.arg(repo).arg(src);
        let out = run_capture(&mut cmd, "git")?;
        if !out.status.success() {
            return Err(InstallError::Git {
                step: "clone",
                status: out.status.code(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
    } else {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(src).arg("fetch").arg("--all");
        let out = run_capture(&mut cmd, "git")?;
        if !out.status.success() {
            warn!(
                stderr = %String::from_utf8_lossy(&out.stderr),
                "git fetch failed — proceeding with existing checkout"
            );
        }
    }

    if let Some(rev) = &resolved.rev {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(src)
            .arg("reset")
            .arg("--hard")
            .arg(rev);
        let out = run_capture(&mut cmd, "git")?;
        if !out.status.success() {
            return Err(InstallError::Git {
                step: "reset --hard <rev>",
                status: out.status.code(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
    }

    // Record HEAD so the stamp covers the actually-built commit.
    let mut head = Command::new("git");
    head.arg("-C").arg(src).arg("rev-parse").arg("HEAD");
    let out = run_capture(&mut head, "git")?;
    if !out.status.success() {
        return Err(InstallError::Git {
            step: "rev-parse HEAD",
            status: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn run_tree_sitter_build(build_dir: &Path, out_lib: &Path) -> Result<(), InstallError> {
    let mut cmd = Command::new("tree-sitter");
    cmd.arg("build")
        .arg("-o")
        .arg(out_lib)
        .current_dir(build_dir);
    let out = run_capture(&mut cmd, "tree-sitter")?;
    if !out.status.success() {
        return Err(InstallError::Build {
            dir: build_dir.to_path_buf(),
            status: out.status.code(),
            stderr: format!(
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
            .trim()
            .to_string(),
        });
    }
    // Sanity check: tree-sitter sometimes ignores -o on older CLIs and writes
    // somewhere else. If the file isn't where we asked, surface that early
    // rather than later when libloading complains.
    if !out_lib.exists() {
        return Err(InstallError::Build {
            dir: build_dir.to_path_buf(),
            status: out.status.code(),
            stderr: format!(
                "tree-sitter build reported success but {} was not produced",
                out_lib.display()
            ),
        });
    }
    let _ = library_filename();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_falls_back_to_manifest() {
        let m = Manifest::parse(
            r#"
[rust]
repo = "https://example/rust"
extensions = ["rs"]
"#,
        )
        .unwrap();
        let r = resolve("rust", &InstallOpts::default(), &m).unwrap();
        assert_eq!(r.repo.as_deref(), Some("https://example/rust"));
        assert_eq!(r.language, "rust");
        assert_eq!(r.extensions, vec!["rs".to_string()]);
    }

    #[test]
    fn resolve_opts_override_manifest() {
        let m = Manifest::parse(
            r#"
[rust]
repo = "https://example/rust"
extensions = ["rs"]
"#,
        )
        .unwrap();
        let opts = InstallOpts {
            repo: Some("https://fork/rust".into()),
            extensions: Some(vec!["rs".into(), "rust".into()]),
            ..Default::default()
        };
        let r = resolve("rust", &opts, &m).unwrap();
        assert_eq!(r.repo.as_deref(), Some("https://fork/rust"));
        assert_eq!(r.extensions, vec!["rs".to_string(), "rust".to_string()]);
    }

    #[test]
    fn resolve_unknown_without_repo_errors() {
        let m = Manifest::parse("").unwrap();
        let err = resolve("nope", &InstallOpts::default(), &m).unwrap_err();
        assert!(matches!(err, InstallError::UnknownGrammar { .. }));
    }

    #[test]
    fn resolve_unknown_with_path_succeeds() {
        let m = Manifest::parse("").unwrap();
        let opts = InstallOpts {
            path: Some(PathBuf::from("/tmp/foo")),
            extensions: Some(vec!["foo".into()]),
            ..Default::default()
        };
        let r = resolve("foo", &opts, &m).unwrap();
        assert!(r.path.is_some());
        assert_eq!(r.language, "foo");
    }
}
