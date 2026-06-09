//! Editor-side registry of running LSP clients, indexed by symbolic name.
//!
//! The `LspRuntime` (on the tokio thread) is the canonical owner of
//! `ClientHandle`s. The editor only needs to remember:
//! - `name → LspClientId` so a second buffer of the same language reuses
//!   the same server,
//! - the negotiated `Encoding` per client so position conversion can run
//!   synchronously on the editor side.
//!
//! Spawning blocks the editor briefly because `initialize` is fast and
//! returning the `ClientId` is the simplest way to make the rest of the
//! editor's flow synchronous.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crossbeam_channel::bounded;
use rizz_actions::LspClientId;
use rizz_lsp_install::ServerSpec;

use crate::event::{RuntimeCmd, SpawnReply};
use crate::position::Encoding;
use crate::runtime::runtime;

#[derive(Debug, Clone, Copy)]
pub struct RunningClient {
    pub id: LspClientId,
    pub encoding: Encoding,
}

#[derive(Default)]
pub struct LspRegistry {
    by_name: HashMap<String, RunningClient>,
}

impl LspRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, name: &str) -> Option<RunningClient> {
        self.by_name.get(name).copied()
    }

    /// Spawn a new client task (or return the existing entry) and wait up
    /// to 5s for the `initialize` handshake. Returns the `LspClientId` the
    /// editor uses for subsequent requests.
    pub fn ensure_running(
        &mut self,
        name: &str,
        binary: PathBuf,
        spec: ServerSpec,
        root_uri: Option<String>,
    ) -> Result<RunningClient, String> {
        if let Some(c) = self.by_name.get(name) {
            return Ok(*c);
        }
        let (reply_tx, reply_rx) = bounded::<SpawnReply>(1);
        runtime().send_cmd(RuntimeCmd::SpawnClient {
            name: name.to_string(),
            binary,
            spec,
            root_uri,
            reply: reply_tx,
        });
        match reply_rx.recv_timeout(Duration::from_secs(6)) {
            Ok(SpawnReply::Ok { client, encoding }) => {
                let r = RunningClient {
                    id: client,
                    encoding,
                };
                self.by_name.insert(name.to_string(), r);
                Ok(r)
            }
            Ok(SpawnReply::Err(msg)) => Err(msg),
            Err(_) => Err("lsp runtime did not reply within 6s".to_string()),
        }
    }

    pub fn shutdown(&mut self, name: &str) {
        if let Some(c) = self.by_name.remove(name) {
            runtime().send_cmd(RuntimeCmd::Shutdown { client: c.id });
        }
    }

    pub fn shutdown_all(&mut self) {
        for (_, c) in self.by_name.drain() {
            runtime().send_cmd(RuntimeCmd::Shutdown { client: c.id });
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, RunningClient)> {
        self.by_name.iter().map(|(k, v)| (k.as_str(), *v))
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }

    /// Forget a client without sending shutdown. Used when the runtime
    /// reports `ServerExited` — the dispatch state is already gone on the
    /// tokio side.
    pub fn forget(&mut self, id: LspClientId) {
        self.by_name.retain(|_, c| c.id != id);
    }
}

impl std::fmt::Debug for LspRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspRegistry")
            .field("clients", &self.by_name.len())
            .finish()
    }
}
