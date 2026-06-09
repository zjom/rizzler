//! Process-wide tokio executor for the LSP subsystem.
//!
//! Created lazily on first access (`runtime()`). One dedicated `std::thread`
//! drives a multi-threaded tokio runtime; the editor talks to it over
//! crossbeam channels.
//!
//! The dispatcher task owns the `ClientRegistry` and is the single owner of
//! every spawned `ClientHandle`. The editor calls `runtime().send_cmd(...)`
//! to enqueue a `RuntimeCmd`; the dispatcher routes it to the matching
//! client's `ClientCmd` channel.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crossbeam_channel::{Receiver, Sender};
use rizz_actions::LspClientId;
use tokio::runtime::Builder;
use tracing::{error, info, warn};

use crate::client::{self, ClientCmd, ClientHandle};
use crate::event::{LspEvent, RuntimeCmd, SpawnReply};

pub struct LspRuntime {
    cmd_tx: Sender<RuntimeCmd>,
    pub events_rx: Receiver<LspEvent>,
}

impl LspRuntime {
    /// Editor-side handle: send a command into the tokio dispatcher.
    pub fn send_cmd(&self, cmd: RuntimeCmd) {
        if let Err(e) = self.cmd_tx.send(cmd) {
            warn!(error = %e, "lsp runtime channel send failed (runtime gone?)");
        }
    }

    pub fn events_rx(&self) -> &Receiver<LspEvent> {
        &self.events_rx
    }
}

static RUNTIME: OnceLock<LspRuntime> = OnceLock::new();

/// Lazily initialize and return the process-wide LSP runtime.
pub fn runtime() -> &'static LspRuntime {
    RUNTIME.get_or_init(start_runtime)
}

fn start_runtime() -> LspRuntime {
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<RuntimeCmd>();
    let (events_tx, events_rx) = crossbeam_channel::unbounded::<LspEvent>();
    let events_tx_for_thread = events_tx.clone();
    std::thread::Builder::new()
        .name("rizz-lsp".to_string())
        .spawn(move || {
            let rt = match Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .thread_name("rizz-lsp-worker")
                .build()
            {
                Ok(r) => r,
                Err(e) => {
                    error!(error = %e, "failed to start lsp tokio runtime");
                    return;
                }
            };
            rt.block_on(dispatcher(cmd_rx, events_tx_for_thread));
            info!("lsp runtime thread exiting");
        })
        .expect("failed to spawn lsp runtime thread");
    LspRuntime { cmd_tx, events_rx }
}

async fn dispatcher(cmd_rx: Receiver<RuntimeCmd>, events_tx: Sender<LspEvent>) {
    let clients: Mutex<HashMap<LspClientId, ClientHandle>> = Mutex::new(HashMap::new());
    let mut next_client_id: u64 = 1;

    loop {
        // The dispatcher itself runs in tokio but it sleeps on a blocking
        // crossbeam_channel recv — keep the wait off the worker pool via
        // `spawn_blocking`. The channel send rate is low so the indirection
        // doesn't matter.
        let cmd = tokio::task::spawn_blocking({
            let cmd_rx = cmd_rx.clone();
            move || cmd_rx.recv()
        })
        .await;
        let Ok(Ok(cmd)) = cmd else {
            info!("lsp dispatcher channel closed");
            break;
        };
        match cmd {
            RuntimeCmd::SpawnClient {
                name,
                binary,
                spec,
                root_uri,
                reply,
            } => {
                let id = LspClientId(next_client_id);
                next_client_id += 1;
                let events_tx = events_tx.clone();
                let spawn_res =
                    client::spawn(id, name.clone(), &binary, spec, root_uri, events_tx).await;
                match spawn_res {
                    Ok(handle) => {
                        let encoding = handle.encoding;
                        clients.lock().unwrap().insert(id, handle);
                        let _ = reply.send(SpawnReply::Ok { client: id, encoding });
                    }
                    Err(e) => {
                        warn!(name, error = %e, "lsp client spawn failed");
                        let _ = reply.send(SpawnReply::Err(e.to_string()));
                    }
                }
            }
            RuntimeCmd::Shutdown { client } => {
                let handle = clients.lock().unwrap().remove(&client);
                if let Some(h) = handle {
                    let _ = h.cmd_tx.send(ClientCmd::Shutdown);
                }
            }
            other => dispatch_to_client(&clients, other),
        }
    }
}

fn dispatch_to_client(
    clients: &Mutex<HashMap<LspClientId, ClientHandle>>,
    cmd: RuntimeCmd,
) {
    let (client_id, client_cmd) = match cmd {
        RuntimeCmd::DidOpen {
            client,
            uri,
            language_id,
            version,
            text,
        } => (
            client,
            ClientCmd::DidOpen {
                uri,
                language_id,
                version,
                text,
            },
        ),
        RuntimeCmd::DidChange {
            client,
            uri,
            version,
            changes,
        } => (
            client,
            ClientCmd::DidChange {
                uri,
                version,
                changes,
            },
        ),
        RuntimeCmd::DidClose { client, uri } => (client, ClientCmd::DidClose { uri }),
        RuntimeCmd::Hover {
            client,
            seq,
            uri,
            position,
        } => (
            client,
            ClientCmd::Hover {
                seq,
                uri,
                position,
            },
        ),
        RuntimeCmd::GotoDefinition {
            client,
            seq,
            uri,
            position,
        } => (
            client,
            ClientCmd::GotoDefinition {
                seq,
                uri,
                position,
            },
        ),
        RuntimeCmd::Completion {
            client,
            seq,
            uri,
            position,
        } => (
            client,
            ClientCmd::Completion {
                seq,
                uri,
                position,
            },
        ),
        RuntimeCmd::Format {
            client,
            seq,
            uri,
            tab_size,
            insert_spaces,
        } => (
            client,
            ClientCmd::Format {
                seq,
                uri,
                tab_size,
                insert_spaces,
            },
        ),
        RuntimeCmd::CodeAction {
            client,
            seq,
            uri,
            range,
        } => (
            client,
            ClientCmd::CodeAction {
                seq,
                uri,
                range,
            },
        ),
        RuntimeCmd::ExecuteCommand {
            client,
            seq,
            command,
            arguments,
        } => (
            client,
            ClientCmd::ExecuteCommand {
                seq,
                command,
                arguments,
            },
        ),
        RuntimeCmd::Cancel { client, seq } => (client, ClientCmd::Cancel { seq }),
        RuntimeCmd::Shutdown { .. } | RuntimeCmd::SpawnClient { .. } => return,
    };
    let guard = clients.lock().unwrap();
    if let Some(handle) = guard.get(&client_id) {
        let _ = handle.cmd_tx.send(client_cmd);
    } else {
        warn!(?client_id, "no such lsp client");
    }
}

