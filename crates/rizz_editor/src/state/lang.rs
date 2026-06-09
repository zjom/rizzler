//! Tree-sitter grammar and LSP server install / attach.
//!
//! The two paths mirror each other closely — both consult a manifest, both
//! cache a one-time warn / failed-install marker per language. PR2 will
//! extract the shared shape into a generic `LanguageBackend<I>` and a
//! `rizz_install` crate; for now this module is the duplication's home.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use rizz_lsp::LspBufferAttachment;
use rizz_lsp_install::{InstallOpts as LspInstallOpts, install as install_lsp_server, try_load_cached as try_load_cached_lsp};
use rizz_text::BufferId;
use rizz_ts_install::InstallOpts;
use tracing::{error, info, instrument, warn};

use super::workspace::{find_workspace_root, path_to_uri};
use super::State;

impl State {
    /// Register a runtime-loaded tree-sitter grammar from a shared library
    /// (`.so` / `.dylib` / `.dll`). Pre-flights the grammar+query by building
    /// a throwaway highlighter — if that errors, the registry isn't touched,
    /// so a bad call doesn't silently break future buffer loads. After
    /// registration, any already-open buffer whose extension matches and has
    /// no highlighter attached gets one installed in place.
    #[instrument(skip(self, highlights_query), fields(
        library_path = %library_path.display(),
        query_bytes = highlights_query.len(),
    ))]
    pub fn register_grammar(
        &mut self,
        name: &str,
        extensions: &[String],
        library_path: &Path,
        highlights_query: &str,
    ) -> Result<(), rizz_ts::TsError> {
        if let Err(e) = self
            .ts_registry
            .register(name, extensions, library_path, highlights_query)
        {
            error!(error = %e, "register_grammar failed");
            return Err(e);
        }
        info!(?extensions, "registered grammar");
        let ids: Vec<BufferId> = self.bufs.iter().map(|(id, _)| id).collect();
        for id in ids {
            self.install_highlighter(id);
        }
        Ok(())
    }

    /// Declarative grammar install. Resolves the name against the curated
    /// manifest plus per-call opts, fetches the source (via `git`) and builds
    /// it (via the user's `tree-sitter` CLI) when no matching cache stamp is
    /// present, then registers the resulting library with [`Self::register_grammar`].
    /// Idempotent: a matching cache short-circuits the shell-outs.
    #[instrument(skip(self, opts), fields(name = name))]
    pub fn install_grammar(&mut self, name: &str, opts: InstallOpts) -> anyhow::Result<()> {
        let installed = rizz_ts_install::install(name, &opts, &self.grammar_manifest)
            .map_err(|e| anyhow::anyhow!(e))?;
        let highlights = rizz_ts_install::read_highlights(&installed)?;
        self.register_grammar(
            &installed.language,
            &installed.extensions,
            &installed.library,
            &highlights,
        )
        .map_err(|e| anyhow::anyhow!(e))?;
        // Reset one-shot warning + failed-install markers so a later
        // uninstall+reinstall cycle can warn or retry again.
        let key = Rc::<str>::from(name);
        self.warned_missing_grammars.remove(&key);
        self.failed_auto_installs.remove(&key);
        Ok(())
    }

    /// True when the grammar cache holds a parser library + highlights query
    /// for `name`. Pure local check; never touches the network. Useful for
    /// `(if (not (grammar-installed? 'rust)) (grammar-install 'rust))`.
    pub fn grammar_installed(&self, name: &str) -> bool {
        rizz_ts_install::try_load_cached(name, &InstallOpts::default(), &self.grammar_manifest)
            .is_some()
    }

    /// True when opening a file with a known extension should auto-install
    /// the corresponding tree-sitter grammar. Toggled via the lisp
    /// `(set-grammar-auto-install …)` builtin.
    pub fn grammar_auto_install(&self) -> bool {
        self.grammar_auto_install
    }

    /// Set the auto-install flag. When toggled off, opening a file whose
    /// grammar is not yet cached reverts to the old behavior — a one-time
    /// notify pointing the user at `(grammar-install '<name>)`.
    pub fn set_grammar_auto_install(&mut self, on: bool) {
        self.grammar_auto_install = on;
    }

    /// If `buf` is a file buffer whose extension matches a registered
    /// dynamic grammar and no highlighter is currently attached, install one.
    /// A buffer that already has a (native) highlighter is left alone.
    ///
    /// When the extension is unknown to the registry but the curated manifest
    /// names a grammar for it, try to register it from the on-disk cache (no
    /// network). If the cache is empty and `grammar_auto_install` is set,
    /// shell out via [`Self::install_grammar`] to fetch and build it once.
    /// Otherwise surface a one-time notify pointing the user at
    /// `(grammar-install '<name>)`.
    pub(super) fn install_highlighter(&mut self, buf: BufferId) {
        if !self.bufs.contains(buf) {
            return;
        }
        if self.bufs[buf].highlight().is_some() {
            return;
        }
        let Some(path) = self.bufs[buf].fs_path() else {
            return;
        };
        if let Some(h) = self.ts_registry.highlighter_for_path(&path) {
            self.bufs[buf].set_highlighter(Some(h));
            return;
        }
        let Some(ext) = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
        else {
            return;
        };
        let Some(grammar_name) = self
            .grammar_manifest
            .lookup_by_ext(&ext)
            .map(str::to_string)
        else {
            return;
        };
        if let Some(installed) = rizz_ts_install::try_load_cached(
            &grammar_name,
            &InstallOpts::default(),
            &self.grammar_manifest,
        ) {
            match rizz_ts_install::read_highlights(&installed) {
                Ok(highlights) => {
                    if let Err(e) = self.register_grammar(
                        &installed.language,
                        &installed.extensions,
                        &installed.library,
                        &highlights,
                    ) {
                        warn!(error = %e, name = grammar_name, "auto-load from cache failed");
                    } else if let Some(h) = self.ts_registry.highlighter_for_path(&path) {
                        self.bufs[buf].set_highlighter(Some(h));
                    }
                }
                Err(e) => warn!(error = %e, "could not read cached highlights"),
            }
            return;
        }
        let key: Rc<str> = Rc::from(grammar_name.as_str());
        if self.grammar_auto_install && !self.failed_auto_installs.contains(&key) {
            let msg = format!("installing tree-sitter grammar `{grammar_name}`…");
            self.notify_via_lisp(&msg);
            match self.install_grammar(&grammar_name, InstallOpts::default()) {
                Ok(()) => {
                    // `install_grammar` → `register_grammar` already attaches
                    // the new highlighter to every open buffer.
                }
                Err(e) => {
                    self.failed_auto_installs.insert(key);
                    let msg = format!(
                        "auto-install of `{grammar_name}` failed: {e} — run `(grammar-install '{grammar_name})` manually or `(set-grammar-auto-install nil)` to silence this"
                    );
                    self.notify_via_lisp(&msg);
                }
            }
            return;
        }
        if self.warned_missing_grammars.insert(key) {
            let msg = format!(
                "grammar `{grammar_name}` not installed — run `(grammar-install '{grammar_name})` or `(set-grammar-auto-install t)`"
            );
            self.notify_via_lisp(&msg);
        }
    }

    /// True when a server is cached locally (PATH or recipe-built) for
    /// `name`. Pure local check; never touches the network.
    pub fn lsp_installed(&self, name: &str) -> bool {
        try_load_cached_lsp(name, &self.lsp_manifest).is_some()
    }

    pub fn lsp_auto_install(&self) -> bool {
        self.lsp_auto_install
    }

    pub fn set_lsp_auto_install(&mut self, on: bool) {
        self.lsp_auto_install = on;
    }

    /// Register a server programmatically (low-level — bypasses lsp.toml).
    /// Used by the `(lsp-register)` lisp builtin. After registration, any
    /// already-open buffer whose extension matches and has no LSP attached
    /// gets attached in place — same pattern as `register_grammar`.
    pub fn register_lsp_server(
        &mut self,
        name: &str,
        command: String,
        args: Vec<String>,
        extensions: Vec<String>,
        root_markers: Vec<String>,
    ) {
        let spec = rizz_lsp_install::ServerSpec {
            command,
            args,
            extensions,
            root_markers,
            ..Default::default()
        };
        self.lsp_manifest.insert(name.to_string(), spec);
        let ids: Vec<BufferId> = self.bufs.iter().map(|(id, _)| id).collect();
        for id in ids {
            self.install_lsp_client(id);
        }
    }

    /// Shell out to the install recipe in `lsp.toml` (if any). Returns the
    /// resolved binary path on success. Used by `(lsp-install)`. After
    /// install, retroactively attaches any open buffer whose extension
    /// matches and which has no LSP attached.
    pub fn install_lsp_server(
        &mut self,
        name: &str,
        opts: LspInstallOpts,
    ) -> Result<PathBuf, String> {
        let res = install_lsp_server(name, &opts, &self.lsp_manifest)
            .map(|i| i.binary)
            .map_err(|e| e.to_string());
        if res.is_ok() {
            let ids: Vec<BufferId> = self.bufs.iter().map(|(id, _)| id).collect();
            for id in ids {
                self.install_lsp_client(id);
            }
        }
        res
    }

    /// If `buf` has a known file extension that matches an entry in the
    /// LSP manifest, resolve the server binary, spawn it (or reuse an
    /// existing client), and attach an `LspBufferAttachment` to the buffer.
    /// A buffer that already has an attachment is left alone. Mirrors
    /// [`Self::install_highlighter`] step-for-step.
    pub(crate) fn install_lsp_client(&mut self, buf: BufferId) {
        if !self.bufs.contains(buf) {
            return;
        }
        if self.bufs[buf].lsp_handle().is_some() {
            return;
        }
        let Some(path) = self.bufs[buf].fs_path() else {
            return;
        };
        let Some(ext) = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
        else {
            return;
        };
        let Some(server_name) = self.lsp_manifest.lookup_by_ext(&ext).map(str::to_string) else {
            return;
        };

        let mut installed = try_load_cached_lsp(&server_name, &self.lsp_manifest);
        let key: Rc<str> = Rc::from(server_name.as_str());
        if installed.is_none() {
            if self.lsp_auto_install && !self.failed_lsp_auto_installs.contains(&key) {
                self.notify_via_lisp(&format!("installing lsp server `{server_name}`…"));
                match install_lsp_server(
                    &server_name,
                    &LspInstallOpts::default(),
                    &self.lsp_manifest,
                ) {
                    Ok(i) => installed = Some(i),
                    Err(e) => {
                        self.failed_lsp_auto_installs.insert(key.clone());
                        self.notify_via_lisp(&format!(
                            "auto-install of `{server_name}` failed: {e} — run `(lsp-install '{server_name})` manually or `(set-lsp-auto-install nil)` to silence this"
                        ));
                        return;
                    }
                }
            } else if self.warned_missing_servers.insert(key) {
                self.notify_via_lisp(&format!(
                    "lsp server `{server_name}` not installed — run `(lsp-install '{server_name})` or `(set-lsp-auto-install t)`"
                ));
                return;
            } else {
                return;
            }
        }
        let installed = match installed {
            Some(i) => i,
            None => return,
        };

        let root_dir = find_workspace_root(&path, &installed.spec.root_markers)
            .unwrap_or(self.workdir.to_path_buf());
        let root_uri = path_to_uri(&root_dir);

        let running = match self.lsp_registry.ensure_running(
            &server_name,
            installed.binary.clone(),
            installed.spec.clone(),
            root_uri,
        ) {
            Ok(r) => r,
            Err(e) => {
                warn!(server = server_name, error = %e, "ensure_running failed");
                self.notify_via_lisp(&format!("lsp `{server_name}` failed to start: {e}"));
                return;
            }
        };

        let Some(uri) = path_to_uri(&path) else {
            warn!(?path, "buffer path is not utf-8; skipping lsp attach");
            return;
        };

        let language_id = installed
            .spec
            .extensions
            .first()
            .cloned()
            .unwrap_or_else(|| ext.clone());
        let attachment =
            LspBufferAttachment::new(running.id, uri.clone(), language_id, running.encoding);
        self.buf_by_uri.insert(uri, buf);
        self.bufs[buf].set_lsp_handle(Some(Box::new(attachment)));
    }
}
