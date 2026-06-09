//! Resolve a server binary by name: prefer `$PATH`, then the per-server
//! cache, then run the install recipe if one is configured.

use std::path::PathBuf;
use std::process::Command;

use sha2::{Digest, Sha256};
use tracing::{debug, info, instrument, warn};

use crate::cache;
use crate::manifest::{Manifest, ServerSpec};
use crate::InstallError;

/// Per-call overrides layered on top of the manifest entry. Everything
/// is optional; `(lsp-install 'rust-analyzer)` uses `InstallOpts::default()`.
#[derive(Debug, Default, Clone)]
pub struct InstallOpts {
    pub force: bool,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub install: Option<String>,
}

/// Successful resolution: a binary path plus the spec to hand the LSP
/// client task for `initialize`.
#[derive(Debug, Clone)]
pub struct InstalledServer {
    pub name: String,
    pub binary: PathBuf,
    pub spec: ServerSpec,
}

fn resolve(name: &str, opts: &InstallOpts, manifest: &Manifest) -> Result<ServerSpec, InstallError> {
    let mut spec = manifest.get(name).cloned().unwrap_or_default();
    if spec.command.is_empty() {
        if let Some(cmd) = &opts.command {
            spec.command = cmd.clone();
        } else {
            return Err(InstallError::UnknownServer {
                name: name.to_string(),
            });
        }
    }
    if let Some(cmd) = &opts.command {
        spec.command = cmd.clone();
    }
    if let Some(args) = &opts.args {
        spec.args = args.clone();
    }
    if let Some(install) = &opts.install {
        spec.install = Some(install.clone());
    }
    Ok(spec)
}

fn stamp_for(spec: &ServerSpec) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"command=");
    hasher.update(spec.command.as_bytes());
    hasher.update(b"\nrecipe=");
    hasher.update(spec.install.as_deref().unwrap_or("").as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Resolve a server binary: `$PATH` first, then the per-server cache,
/// then the install recipe. A matching cache stamp short-circuits the
/// recipe, so repeated calls are idempotent.
#[instrument(skip(opts, manifest), fields(name = name))]
pub fn install(
    name: &str,
    opts: &InstallOpts,
    manifest: &Manifest,
) -> Result<InstalledServer, InstallError> {
    let spec = resolve(name, opts, manifest)?;

    if !opts.force {
        if let Some(binary) = which::which(&spec.command).ok() {
            debug!(name, ?binary, "found on PATH");
            return Ok(InstalledServer {
                name: name.to_string(),
                binary,
                spec,
            });
        }
    }

    let cache_root = cache::cache_root();
    let server_dir = cache::server_dir(&cache_root, name);
    let binary = cache::binary_path(&cache_root, name, &spec.command);
    let stamp = cache::stamp_path(&cache_root, name);
    let want_stamp = stamp_for(&spec);

    if !opts.force
        && binary.exists()
        && std::fs::read_to_string(&stamp)
            .ok()
            .as_deref()
            .map(str::trim)
            == Some(want_stamp.as_str())
    {
        debug!(name, ?binary, "cache hit");
        return Ok(InstalledServer {
            name: name.to_string(),
            binary,
            spec,
        });
    }

    let Some(recipe) = spec.install.clone() else {
        return Err(InstallError::NotOnPathAndNoRecipe {
            name: name.to_string(),
            command: spec.command.clone(),
        });
    };

    std::fs::create_dir_all(cache::bin_dir(&cache_root, name)).map_err(|source| {
        InstallError::Io {
            path: cache::bin_dir(&cache_root, name),
            source,
        }
    })?;
    std::fs::create_dir_all(cache::log_path(&cache_root, name).parent().unwrap()).map_err(
        |source| InstallError::Io {
            path: cache::log_path(&cache_root, name),
            source,
        },
    )?;

    info!(name, "running install recipe");
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(&recipe)
        .env("RIZZ_LSP_DIR", &server_dir)
        .current_dir(&server_dir);
    let out = cmd.output().map_err(|source| InstallError::Io {
        path: PathBuf::from("sh"),
        source,
    })?;
    let log_text = format!(
        "exit: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}\n",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    if let Err(e) = std::fs::write(cache::log_path(&cache_root, name), &log_text) {
        warn!(error = %e, "could not write install log");
    }
    if !out.status.success() {
        return Err(InstallError::Recipe {
            name: name.to_string(),
            status: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    if !binary.exists() {
        return Err(InstallError::RecipeMissingOutput {
            name: name.to_string(),
            expected: binary,
        });
    }
    std::fs::write(&stamp, &want_stamp).map_err(|source| InstallError::Io {
        path: stamp,
        source,
    })?;
    info!(name, ?binary, "installed");
    Ok(InstalledServer {
        name: name.to_string(),
        binary,
        spec,
    })
}

/// Pure cache + `$PATH` lookup; never runs the recipe. Used by the
/// auto-attach hook so opening a buffer never blocks on a recipe. On
/// `None`, the caller decides whether to invoke [`install`] or notify.
pub fn try_load_cached(name: &str, manifest: &Manifest) -> Option<InstalledServer> {
    let spec = manifest.get(name)?.clone();
    if let Ok(binary) = which::which(&spec.command) {
        return Some(InstalledServer {
            name: name.to_string(),
            binary,
            spec,
        });
    }
    let cache_root = cache::cache_root();
    let binary = cache::binary_path(&cache_root, name, &spec.command);
    if !binary.exists() {
        return None;
    }
    let stamp = cache::stamp_path(&cache_root, name);
    let want_stamp = stamp_for(&spec);
    if std::fs::read_to_string(&stamp)
        .ok()
        .as_deref()
        .map(str::trim)
        != Some(want_stamp.as_str())
    {
        // Stale stamp is not enough to discard the cached binary — a
        // recipe-text edit shouldn't force a rebuild on buffer open.
        // `install --force` is the way to rebuild.
        debug!(name, "stale stamp, returning cached binary anyway");
    }
    Some(InstalledServer {
        name: name.to_string(),
        binary,
        spec,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_server_when_not_in_manifest_and_no_command_opt() {
        let m = Manifest::default();
        let err = install("nope", &InstallOpts::default(), &m).unwrap_err();
        assert!(matches!(err, InstallError::UnknownServer { .. }));
    }

    #[test]
    fn opts_command_override_lets_unknown_name_resolve_to_path() {
        // `sh` is reliably on PATH on macOS/Linux test runners.
        let m = Manifest::default();
        let installed = install(
            "adhoc",
            &InstallOpts {
                command: Some("sh".into()),
                ..Default::default()
            },
            &m,
        )
        .unwrap();
        assert_eq!(installed.name, "adhoc");
        assert!(installed.binary.file_name().is_some());
    }
}
