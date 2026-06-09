//! Workspace & config — workdir, config dir, init script, manifest seeding.
//!
//! The config dir holds `init.rz`, `grammars.toml`, and `lsp.toml`. Each is
//! seeded from an embedded copy on first launch so a fresh checkout lands on
//! a working editor. Manifest reads degrade to "empty manifest + warn" on a
//! parse error — a broken file should never block boot.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use rizz::RizzError;
use rizz_lsp_install::Manifest as LspManifest;
use rizz_ts_install::Manifest as GrammarManifest;
use tracing::{debug, error, info, instrument, warn};

use super::State;

pub(super) const INIT_SCRIPT_NAME: &str = "init.rz";
pub(super) const EMBEDDED_INIT_SCRIPT: &str = include_str!("../../../../init.rz");
pub(super) const GRAMMARS_MANIFEST_NAME: &str = "grammars.toml";
pub(super) const EMBEDDED_GRAMMARS_MANIFEST: &str = include_str!("../../../../grammars.toml");
pub(super) const LSP_MANIFEST_NAME: &str = "lsp.toml";
pub(super) const EMBEDDED_LSP_MANIFEST: &str = include_str!("../../../../lsp.toml");

pub(super) fn resolve_workdir(path: Option<&Path>, cwd: &Path) -> PathBuf {
    match path {
        Some(p) if p.is_dir() => p.to_path_buf(),
        Some(p) => p
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| cwd.to_path_buf()),
        None => cwd.to_path_buf(),
    }
}

/// Directory holding `init.rz` when the caller doesn't override it.
/// Debug/test builds use the workspace root (so the checked-in `init.rz` is
/// the edit loop); release builds use `$XDG_CONFIG_HOME/rizz`.
#[cfg(any(test, debug_assertions))]
pub(super) fn default_config_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

#[cfg(all(not(test), not(debug_assertions)))]
pub(super) fn default_config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("rizz")
}

/// Read `<config_dir>/<name>`, seeding it from `embedded` if missing so
/// first-run users land on a working file. Used for both `init.rz` and
/// `grammars.toml`.
fn load_or_seed(config_dir: &Path, name: &str, embedded: &str) -> anyhow::Result<String> {
    use std::fs;
    let path = config_dir.join(name);
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, embedded)?;
    }
    Ok(fs::read_to_string(&path)?)
}

/// Read `<config_dir>/init.rz`, seeding it from the embedded template if it
/// doesn't exist yet so first-run users land on a working config.
pub(super) fn load_init_script_at(config_dir: &Path) -> anyhow::Result<String> {
    load_or_seed(config_dir, INIT_SCRIPT_NAME, EMBEDDED_INIT_SCRIPT)
}

/// Read `<config_dir>/grammars.toml`, seeding it from the embedded copy if
/// missing. A failure to read or parse falls back to an empty manifest with
/// a logged warning — a broken file should never keep the editor from boot.
pub(super) fn load_grammar_manifest(config_dir: &Path) -> GrammarManifest {
    match load_or_seed(
        config_dir,
        GRAMMARS_MANIFEST_NAME,
        EMBEDDED_GRAMMARS_MANIFEST,
    ) {
        Ok(text) => match GrammarManifest::parse(&text) {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "grammars.toml parse failed — falling back to empty manifest");
                GrammarManifest::default()
            }
        },
        Err(e) => {
            warn!(error = %e, "grammars.toml load failed — falling back to empty manifest");
            GrammarManifest::default()
        }
    }
}

/// Read `<config_dir>/lsp.toml`, seeding it from the embedded copy if
/// missing. Same degrade-to-empty semantics as the grammar manifest.
pub(super) fn load_lsp_manifest(config_dir: &Path) -> LspManifest {
    match load_or_seed(config_dir, LSP_MANIFEST_NAME, EMBEDDED_LSP_MANIFEST) {
        Ok(text) => match LspManifest::parse(&text) {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "lsp.toml parse failed — falling back to empty manifest");
                LspManifest::default()
            }
        },
        Err(e) => {
            warn!(error = %e, "lsp.toml load failed — falling back to empty manifest");
            LspManifest::default()
        }
    }
}

/// Turn an absolute filesystem path into a `file://` URI. Returns `None`
/// when the path contains non-UTF-8 bytes we can't url-encode.
pub(super) fn path_to_uri(path: &Path) -> Option<String> {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let s = abs.to_str()?;
    if cfg!(windows) {
        Some(format!("file:///{}", s.replace('\\', "/")))
    } else {
        Some(format!("file://{s}"))
    }
}

/// Convert a `file://...` URI back into a filesystem path. Returns `None`
/// for non-`file` schemes (the editor can't open remote URIs).
pub(super) fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let stripped = if cfg!(windows) {
        rest.trim_start_matches('/').replace('/', "\\")
    } else {
        rest.to_string()
    };
    Some(PathBuf::from(stripped))
}

/// Walk upwards from `start` looking for the first directory that
/// contains any of `markers`. Falls back to `start.parent()`.
pub(super) fn find_workspace_root(start: &Path, markers: &[String]) -> Option<PathBuf> {
    let dir = if start.is_dir() {
        start.to_path_buf()
    } else {
        start.parent()?.to_path_buf()
    };
    let mut current = Some(dir.as_path());
    while let Some(d) = current {
        for m in markers {
            if d.join(m).exists() {
                return Some(d.to_path_buf());
            }
        }
        current = d.parent();
    }
    Some(dir)
}

#[cfg(any(test, debug_assertions))]
pub(super) fn init_eval_err(e: RizzError) -> anyhow::Error {
    panic!("init.rz eval failed: {e}");
}

#[cfg(all(not(test), not(debug_assertions)))]
pub(super) fn init_eval_err(e: RizzError) -> anyhow::Error {
    anyhow::anyhow!(e.to_string())
}

impl State {
    /// Locate `init.rz` under the config dir, seed it from the embedded
    /// template if missing, and eval it with the lisp basedir pointed at the
    /// config dir (so `(open "foo.rz")` inside `init.rz` resolves relative to
    /// it). Restores the basedir to the editor workdir on the way out.
    #[instrument(skip(self), fields(config_dir = %self.config_dir.display()))]
    pub(super) fn run_init_script(&mut self) -> anyhow::Result<()> {
        let src = load_init_script_at(&self.config_dir)?;
        debug!(bytes = src.len(), "loaded init.rz");
        let config_dir = self.config_dir.clone();
        self.lisp.as_mut().unwrap().set_basedir(config_dir.as_ref());
        let eval_result = self.eval_lisp_script(&src);
        self.lisp
            .as_mut()
            .unwrap()
            .set_basedir(self.workdir.as_ref());
        if let Err(e) = &eval_result {
            error!(error = %e, "init.rz eval failed");
        } else {
            info!("init.rz eval ok");
        }
        eval_result.map_err(init_eval_err)
    }

    /// Read `<config_dir>/init.rz` (seeding from the embedded template if
    /// missing). The lisp `reload-config` builtin uses this rather than
    /// re-entering `eval_lisp_script`, since the runtime is already on the
    /// stack when a builtin runs.
    pub fn load_init_script(&self) -> anyhow::Result<String> {
        load_init_script_at(&self.config_dir)
    }

    pub fn config_dir(&self) -> Rc<Path> {
        self.config_dir.clone()
    }

    pub fn init_script_path(&self) -> PathBuf {
        self.config_dir.join(INIT_SCRIPT_NAME)
    }

    pub fn workdir(&self) -> Rc<Path> {
        self.workdir.clone()
    }
}
