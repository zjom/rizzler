//! Headless render-path benchmark. Times the input → apply → precompute
//! pipeline through a `NullRenderer` (no terminal, no widget walk), the part
//! that dominated interactive latency.
//!
//! Run with: `cargo run --release -p rizz_editor --example perf`
//!
//! Scenarios:
//! 1. 500 × `<c-d>` half-page scrolls through a 20k-line buffer
//! 2. 200 × `j` cursor motion in the same buffer
//! 3. 200 chars typed in insert mode
//! 4. picker over 2000 synthetic items: 12 query chars + 200 × `<c-n>`
//!
//! When a locally installed rust grammar is found
//! (`$XDG_DATA_HOME/rizz/grammars/rust`), scenarios 1-3 run against a `.rs`
//! buffer with tree-sitter highlighting attached.

use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rizz_editor::State;
use rizz_editor::state::test_support::test_state;

fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, mods)
}

fn feed(s: &mut State, label: &str, events: Vec<KeyEvent>) {
    let n = events.len();
    let t = Instant::now();
    for e in events {
        s.handle_key_event(e).expect("key event");
    }
    let dt = t.elapsed();
    println!(
        "{label:<46} {n:>5} events  {:>9.1?} total  {:>8.1?}/event",
        dt,
        dt / n as u32
    );
}

fn grammar_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
        })?;
    let dir = base.join("rizz").join("grammars").join("rust");
    dir.join("highlights.scm").exists().then_some(dir)
}

fn main() {
    let mut s = test_state();

    // 20k lines of plausible rust-ish text.
    let mut text = String::new();
    for i in 0..20_000 {
        text.push_str(&format!(
            "fn func_{i}(x: u64) -> u64 {{ x.wrapping_mul({i}) + \"lit\".len() as u64 }}\n"
        ));
    }

    // Try to attach real tree-sitter highlighting via the locally installed
    // rust grammar; fall back to a plain buffer when unavailable.
    let mut highlighted = false;
    if let Some(dir) = grammar_dir() {
        let lib = dir.join(if cfg!(target_os = "macos") {
            "parser.dylib"
        } else {
            "parser.so"
        });
        let query = std::fs::read_to_string(dir.join("highlights.scm")).unwrap_or_default();
        let path = std::env::temp_dir().join("rizz_perf_bench.rs");
        std::fs::write(&path, &text).expect("write bench file");
        if s
            .register_grammar("rust", &["rs".to_string()], &lib, &query)
            .is_ok()
        {
            s.eval_lisp(&format!("(edit \"{}\")", path.display()))
                .expect("open bench file");
            highlighted = true;
        }
    }
    if !highlighted {
        s.handle_paste(text).expect("paste");
    }
    println!(
        "buffer: 20k lines, viewport 120x40, syntax highlighting: {}",
        if highlighted { "on" } else { "off" }
    );

    // Headless: refresh_viewport can't read a terminal size, so pin one.
    s.focused_buf_mut().viewport = rizz_core::Position::new(120, 40);
    s.eval_lisp("(move-cursor 'file-start)").expect("to top");

    feed(
        &mut s,
        "scroll: <c-d> half-page x500",
        vec![key(KeyCode::Char('d'), KeyModifiers::CONTROL); 500],
    );

    s.eval_lisp("(move-cursor 'file-start)").expect("to top");
    feed(
        &mut s,
        "motion: j x200",
        vec![key(KeyCode::Char('j'), KeyModifiers::NONE); 200],
    );

    feed(&mut s, "insert: enter insert mode", vec![key(KeyCode::Char('i'), KeyModifiers::NONE)]);
    feed(
        &mut s,
        "typing: 200 chars in insert mode",
        "let value = compute_something(input, flags); "
            .chars()
            .cycle()
            .take(200)
            .map(|c| key(KeyCode::Char(c), KeyModifiers::NONE))
            .collect(),
    );
    feed(&mut s, "leave insert mode", vec![key(KeyCode::Esc, KeyModifiers::NONE)]);

    // Telescope-style picker over 2000 synthetic items.
    s.eval_lisp(
        r#"(picker-open
             {"title": "bench"
              "items": (fmap (fn _i (n)
                              {"display": (str-join ["src/module_" (to-str n) "/some_longer_file_name.rs"] "")})
                            (range 0 2000))
              "on-accept": (fn _a (it) ())})"#,
    )
    .expect("picker-open");

    feed(
        &mut s,
        "picker: type 12-char query",
        "modulesomefi"
            .chars()
            .map(|c| key(KeyCode::Char(c), KeyModifiers::NONE))
            .collect(),
    );
    feed(
        &mut s,
        "picker: <c-n> x200",
        vec![key(KeyCode::Char('n'), KeyModifiers::CONTROL); 200],
    );
}
