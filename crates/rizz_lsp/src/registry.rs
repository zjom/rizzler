//! Editor-side registry of running LSP clients, indexed by symbolic name.
//!
//! The `LspRuntime` on the tokio thread owns the actual `ClientHandle`s;
//! this registry only remembers `name → LspClientId` (so a second buffer
//! of the same language reuses the server) and the negotiated `Encoding`
//! per client (so position conversion runs synchronously editor-side).

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

    /// Return the existing entry for `name`, or spawn a new client task
    /// and wait up to 6s for the `initialize` handshake.
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

    /// Drop a client without sending `shutdown`. Used on `ServerExited`,
    /// since the tokio-side dispatch state is already gone.
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
