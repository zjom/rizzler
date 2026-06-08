use std::{path::PathBuf, time::Duration};

use crossterm::event::{self, Event};

use rizz_editor::{Config, State};
use rizz_ui::{TerminalGuard, install_panic_hook};
use tracing::{Level, debug, error, info, info_span};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

fn main() -> anyhow::Result<()> {
    let _log_guard = init_tracing();
    info!("rizz starting up");

    install_panic_hook();
    let _guard = TerminalGuard::new()?;
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    debug!(?path, "resolved edit path from argv");

    let mut state = match State::with_config(Config::with_path(path)?) {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "failed to initialize editor state");
            return Err(e);
        }
    };
    state.render()?; // initial render
    loop {
        if state.quit_requested() {
            info!("quit requested, exiting main loop");
            break;
        }

        if event::poll(Duration::from_millis(500))? {
            match event::read()? {
                Event::Key(key_event) => {
                    let _span =
                        info_span!("key", code = ?key_event.code, mods = ?key_event.modifiers)
                            .entered();
                    if let Err(e) = state.handle_key_event(key_event) {
                        error!(error = %e, "handle_key_event failed");
                        return Err(e.into());
                    }
                }
                Event::Paste(text) => {
                    let _span = info_span!("paste", len = text.len()).entered();
                    if let Err(e) = state.handle_paste(text) {
                        error!(error = %e, "handle_paste failed");
                        return Err(e.into());
                    }
                }
                _ => {}
            }
        }
    }

    info!("rizz shutting down");
    Ok(())
}

/// Resolve the log file path, honoring `RIZZ_LOG_FILE` then
/// `$XDG_CACHE_HOME/rizz/rizz.log`, falling back to `~/.cache/rizz/rizz.log`.
fn log_file_path() -> PathBuf {
    if let Some(p) = std::env::var_os("RIZZ_LOG_FILE") {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("rizz").join("rizz.log")
}

/// Set up a file-backed tracing subscriber. Returns the `WorkerGuard` that
/// flushes pending log lines on drop — keep it alive for the lifetime of the
/// process. Logs go to stderr is meaningless for a TUI (we own the alt
/// screen), so everything is routed to a rotating file under the cache dir.
/// Default level is `info`; `RUST_LOG` overrides it (e.g. `RUST_LOG=debug`).
fn init_tracing() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let path = log_file_path();
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        eprintln!("rizz: could not create log dir {parent:?}: {e}");
        return None;
    }
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("rizz: could not open log file {path:?}: {e}");
            return None;
        }
    };
    let (writer, guard) = tracing_appender::non_blocking(file);
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(Level::INFO.to_string()));
    let layer = fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true);
    tracing_subscriber::registry()
        .with(filter)
        .with(layer)
        .init();
    Some(guard)
}
