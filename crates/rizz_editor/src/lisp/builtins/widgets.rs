use std::rc::Rc;

use im::{HashMap as ImHashMap, Vector};
use rizz::runtime::{RuntimeError, Value};
use rizz_ui::styling::normalize_style_value;

use super::super::helpers::{Builtins, as_ident_or_str, as_int, as_str, unit};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    b.be_doc(
        "w-span",
        2,
        |args, _| {
            let text = as_str(&args[0], "w-span")?;
            let style_val = with_editor_mut(|st| {
                let theme = st.theme().borrow();
                normalize_style_value(&args[1], &theme)
            })?;
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(
                Rc::new(Value::Str("text".into())),
                Rc::new(Value::Str(text)),
            );
            if !style_val.is_unit() {
                m.insert(Rc::new(Value::Str("style".into())), style_val);
            }
            Ok(Rc::new(Value::Map(m)))
        },
        r#"(w-span/2)
build a styled span — a map {"text": <str>, "style"?: <style>}. usable both as
a top-level widget (the parser promotes it to a single line) and as one of the
elements in (w-line [...]).
style is one of:
  - ()                              no styling
  - 'face-name | "face-name"        a face name resolved against the theme
  - {"fg": <color> "bg": <color>    inline style; keys: fg, bg, bold,
     "bold": 1 ...}                 italic, underline, reverse, inherit
example:
  (w-span "hello" 'header)
  (w-span "x" {"fg": 'red "bold": 1})
  (w-span "plain" ())"#,
    );

    b.be_doc(
        "set-frame",
        1,
        |args, _| {
            let v = args[0].clone();
            let opt = if v.is_unit() { None } else { Some(v) };
            with_editor_mut(|st| st.set_frame_fn(opt));
            Ok(unit())
        },
        r#"(set-frame/1)
install the per-frame render callback. the fn takes no arguments and returns
the widget tree to render. pass () to clear the installed callback and revert
to the default empty layout.
example:
  (fn _frame ()
    (w-vstack [(w-min-cells 1 (w-editor-tree))
               (w-cells 1 (w-minibuffer))]))
  (set-frame _frame)
  (set-frame ())   ; clear"#,
    );

    b.be_doc(
        "get-frame",
        0,
        |_, _| {
            let f = with_editor_mut(|st| st.get_frame_fn().map(Rc::clone));
            match f {
                Some(f) => Ok(f),
                None => Ok(unit()),
            }
        },
        r#"(get-frame/0)
returns the currently active per-frame render callback fn if set. returns () otherwise."#,
    );

    b.be_doc(
        "w-line",
        1,
        |args, _| {
            let spans: Vec<Rc<Value>> = value_iter(&args[0]).collect();
            let line = widget_line(spans);
            // Optional second arg is the alignment ident: 'left | 'center | 'right.
            if let Some(align_v) = args.get(1) {
                if align_v.is_unit() {
                    return Ok(line);
                }
                let s = match &**align_v {
                    Value::Ident(s) | Value::Str(s) => s.clone(),
                    _ => {
                        return Err(RuntimeError::type_mismatch(
                            "w-line.align",
                            "ident|str ('left|'center|'right)",
                            align_v,
                        ));
                    }
                };
                if !matches!(s.as_ref(), "left" | "center" | "right") {
                    return Err(RuntimeError::type_mismatch(
                        "w-line.align",
                        "'left | 'center | 'right",
                        align_v,
                    ));
                }
                return Ok(widget_set_align(line, &s));
            }
            Ok(line)
        },
        r#"(w-line/1)
build a single-row line widget from a sequence of spans. accepts an optional
2nd arg — the alignment ident 'left | 'center | 'right (defaults to 'left).
spans is an array of span maps — typically results of (w-span ...).
example:
  (w-line [(w-span "left" ())
           (w-span " · " "vague.muted")
           (w-span "right" 'header)])
  (w-line [(w-span "10:42" 'header)] 'right)"#,
    );

    b.be_doc(
        "w-vstack",
        1,
        |args, _| Ok(widget_stack("vertical", &args[0])),
        r#"(w-vstack/1)
build a vertical stack widget. children are laid out top-to-bottom and honour
their outer constraint (see (w-size ...)); unconstrained children default
to Min(1).
children is an array of widgets.
example:
  (w-vstack
    [(w-size 'min   1 (w-editor-tree))
     (w-size 'cells 1 (_status-line))
     (w-size 'cells 1 (w-minibuffer))])"#,
    );

    b.be_doc(
        "w-hstack",
        1,
        |args, _| Ok(widget_stack("horizontal", &args[0])),
        r#"(w-hstack/1)
build a horizontal stack widget. children are laid out left-to-right and honour
their outer constraint (see (w-size ...)); unconstrained children default
to Min(1).
children is an array of widgets.
example:
  (w-hstack
    [(w-size 'min  1 (w-line [(w-span "left" ())]))
     (w-size 'fill 1 (w-line [(w-span "right" ())] 'right))])"#,
    );

    b.be_doc(
        "w-size",
        3,
        |args, _| {
            let kind = as_ident_or_str(&args[0], "w-size.kind")?;
            let kind_str = kind.as_ref();
            // For frac, the call shape is (w-size 'frac n m child) — 4 args
            // total, with m being the denominator. For everything else,
            // (w-size kind n child) — 3 args.
            let (n_raw, m_raw, child) = if kind_str == "frac" {
                let n = as_int(&args[1], "w-size.n")?;
                let Some(m_v) = args.get(2) else {
                    return Err(RuntimeError::type_mismatch(
                        "w-size",
                        "(w-size 'frac n m child) — missing denominator",
                        &args[1],
                    ));
                };
                let m = as_int(m_v, "w-size.m")?;
                let Some(child_v) = args.get(3) else {
                    return Err(RuntimeError::type_mismatch(
                        "w-size",
                        "(w-size 'frac n m child) — missing child",
                        m_v,
                    ));
                };
                (n, m, child_v.clone())
            } else {
                let n = as_int(&args[1], "w-size.n")?;
                (n, 1, args[2].clone())
            };
            let n = n_raw.max(0).min(u16::MAX as i64);
            let m = m_raw.max(1).min(u16::MAX as i64);
            match kind_str {
                "cells" | "min" | "fill" | "frac" => Ok(widget_constrained(kind_str, n, m, child)),
                other => Err(RuntimeError::TypeMismatch {
                    name: "w-size.kind".into(),
                    expected: "'cells | 'min | 'fill | 'frac".into(),
                    got: other.into(),
                }),
            }
        },
        r#"(w-size/3 | /4)
wrap child with a ratatui Constraint. kind picks the constraint flavour:
  'cells    — fixed length of N cells (Constraint::Length)
  'min      — minimum length of N cells, grows to fill leftover (Constraint::Min)
  'fill     — weight N share of the remaining space (Constraint::Fill)
  'frac     — exactly N/M of the parent stack's space (Constraint::Ratio).
              takes one extra arg: (w-size 'frac N M child)
constraints only matter inside (w-vstack ...) / (w-hstack ...); outside a
stack they're ignored. N is clamped to [0, u16::MAX]; M to [1, u16::MAX].
example:
  (w-vstack [(w-size 'cells 1 (_status-line))
             (w-size 'min   1 (w-editor-tree))])
  (w-hstack [(w-size 'frac 1 3 left)
             (w-size 'frac 2 3 right)])"#,
    );

    b.be_doc(
        "w-block",
        2,
        |args, _| {
            let child = args[0].clone();
            let props = match &*args[1] {
                Value::Map(m) => m.clone(),
                Value::Unit => ImHashMap::new(),
                _ => {
                    return Err(RuntimeError::type_mismatch(
                        "w-block.props",
                        "map | ()",
                        &args[1],
                    ));
                }
            };
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(strkey("type"), Rc::new(Value::Str("block".into())));
            m.insert(strkey("child"), child);
            for k in ["border", "title", "face", "border-face", "title-face"] {
                if let Some(v) = props.get(&strkey(k)) {
                    m.insert(strkey(k), v.clone());
                }
            }
            Ok(Rc::new(Value::Map(m)))
        },
        r#"(w-block/2)
wrap child with a bordered/titled box. props is a map (or ()) with optional keys:
  "border":      "none" | "plain" | "rounded" | "double" | "thick"  (default "plain")
  "title":       <str> shown in the top border
  "face":        face name (str|ident) for the content area
  "border-face": face name for the border itself
  "title-face":  face name for the title text
unrecognized keys are silently dropped. omit a key to use its default.
example:
  (w-block (w-line [(w-text "hello" ())])
           {"border":      "rounded"
            "title":       " Status "
            "border-face": "vague.muted"})"#,
    );

    b.be_doc(
        "w-overlay",
        2,
        |args, _| {
            let placement = args[0].clone();
            let child = args[1].clone();
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(strkey("type"), Rc::new(Value::Str("overlay".into())));
            m.insert(strkey("placement"), placement);
            m.insert(strkey("child"), child);
            Ok(Rc::new(Value::Map(m)))
        },
        r#"(w-overlay/2)
paint child on top of the rest of the frame at placement. an overlay has
no backing buffer and never receives keys — it is pure decoration. use it
for floating status indicators, toast notifications, hover hints, or any
chrome that should sit above the editor without stealing focus.

placement is the same shape as the "placement" key in (popup-show ...):
  'centered | 'full
  {"kind": "centered" "w": <dim> "h": <dim>}
  {"kind": "at"       "x": N "y": N "w": <dim> "h": <dim>}
  {"kind": "side"     "side": 'top|'bottom|'left|'right "size": <dim>}
  {"kind": "full"}
<dim> is an int (cells), a float (fraction of parent), or 'fit (sized to
the child's natural area — for overlay this falls back to the available
area since there's no backing buffer to measure).

child is any widget — the entire vocabulary of (w-block), (w-vstack),
(w-line), (w-buffer-view BUFID), … is available.

placement is resolved against the rect the overlay sits inside, so
(w-overlay 'centered ...) at the top of the frame fn centers on the
terminal, while the same call nested in a panel centers in that panel.

example:
  (if saving?
      (w-overlay {"kind": "side" "side": "top" "size": 1}
                 (w-line [(w-span " saving… " 'header)] 'center))
      (w-empty))"#,
    );

    b.be_doc(
        "w-editor-tree",
        0,
        |_, _| {
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(strkey("type"), Rc::new(Value::Str("editor-tree".into())));
            Ok(Rc::new(Value::Map(m)))
        },
        r#"(w-editor-tree/0)
the leaf widget that the renderer expands into the current window tree
(with all open splits and their buffers). takes no arguments — the gutter
is configured separately via (set-gutter fn width), since it's a per-
buffer concern, not a per-layout one.
example:
  (w-editor-tree)"#,
    );

    b.be_doc(
        "set-gutter",
        2,
        |args, _| {
            let f = args[0].clone();
            let width = match args[1].as_int() {
                Some(n) => n.max(0).min(u16::MAX as i64) as u16,
                None => {
                    return Err(RuntimeError::type_mismatch(
                        "set-gutter.width",
                        "int",
                        &args[1],
                    ));
                }
            };
            let fn_opt = if f.is_unit() { None } else { Some(f) };
            with_editor_mut(|st| st.set_gutter(fn_opt, width));
            Ok(unit())
        },
        r#"(set-gutter/2)
install the per-row gutter callback used by every file buffer. fn is called
once per visible row with either the file row number (int) or () for rows
past EOF; it must return a widget — typically (w-text ...). width is the
number of columns reserved for the gutter on the left of the buffer view.
pass () for fn to disable the gutter entirely.
example:
  (fn _gutter (n)
    (if (= n ())
        (w-text "     " "vague.gutter")
        (w-text (str-join [" " (to-str n) " "] "") "vague.gutter")))
  (set-gutter _gutter 5)
  (set-gutter () 0)   ; disable"#,
    );

    b.be_doc(
        "w-minibuffer",
        0,
        |_, _| {
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(strkey("type"), Rc::new(Value::Str("minibuffer".into())));
            Ok(Rc::new(Value::Map(m)))
        },
        r#"(w-minibuffer/0)
the single-row minibuffer leaf widget. put it somewhere in your frame tree
(typically the bottom row of an outer (w-vstack ...)) so command-mode input
and notifications have a place to render.
example:
  (w-vstack [(w-min-cells 1 (w-editor-tree))
             (w-cells 1 (w-minibuffer))])"#,
    );

    b.be_doc(
        "w-empty",
        0,
        |_, _| {
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(strkey("type"), Rc::new(Value::Str("empty".into())));
            Ok(Rc::new(Value::Map(m)))
        },
        r#"(w-empty/0)
a widget that draws nothing. useful as a placeholder, or as the alternate branch
of a conditional that must return a widget but should render no content.
example:
  (fn _maybe-status () (if show-status (_status-line) (w-empty)))"#,
    );

    b.be_doc(
        "w-buffer-view",
        0,
        |args, _| {
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(strkey("type"), Rc::new(Value::Str("buffer-view".into())));
            if let Some(arg) = args.first() {
                let bufno = as_int(arg, "w-buffer-view.bufno")?.max(0);
                m.insert(strkey("bufno"), Rc::new(Value::Int(bufno)));
            }
            Ok(Rc::new(Value::Map(m)))
        },
        r#"(w-buffer-view/0)
renders a single editor buffer into its allocated rect.

two call shapes:

  (w-buffer-view)         — implicit. resolves at render time to the buf of
                            the enclosing (popup-open ...) call. this is the
                            shape you want when constructing the widget tree
                            passed to popup-open; the renderer fills in the
                            correct bufid for you.

  (w-buffer-view BUFID)   — explicit. renders that specific buffer. use this
                            outside a popup, or inside a popup when you want
                            to display a buffer other than the popup's own
                            (e.g. (w-buffer-view (popup-bufno)) reads as
                            "the topmost open popup's buffer" — handy for
                            preview panes).

note that the implicit form is a no-op outside a popup — there's no
"enclosing buffer" for a w-buffer-view that appears in the main frame fn.
prefer explicit bufids there.

example:
  (popup-open
    (w-block (w-buffer-view) {"face": "popup.default"})  ; implicit, ok
    {"text": "hi"})
  (w-buffer-view some-bufid)                              ; explicit"#,
    );
}

fn strkey(s: &str) -> Rc<Value> {
    Rc::new(Value::Str(s.into()))
}

fn value_iter(v: &Rc<Value>) -> Box<dyn Iterator<Item = Rc<Value>> + '_> {
    match &**v {
        Value::Array(xs) => Box::new(xs.iter().cloned().collect::<Vec<_>>().into_iter()),
        Value::Unit => Box::new(std::iter::empty()),
        _ => Box::new(Value::iter(v)),
    }
}

fn widget_line(spans: Vec<Rc<Value>>) -> Rc<Value> {
    let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
    m.insert(strkey("type"), Rc::new(Value::Str("line".into())));
    m.insert(strkey("spans"), Rc::new(Value::Array(spans.into())));
    Rc::new(Value::Map(m))
}

fn widget_set_align(v: Rc<Value>, align: &str) -> Rc<Value> {
    if let Value::Map(m) = &*v {
        let mut m = m.clone();
        m.insert(strkey("align"), Rc::new(Value::Str(align.into())));
        Rc::new(Value::Map(m))
    } else {
        v
    }
}

fn widget_stack(dir: &str, children: &Rc<Value>) -> Rc<Value> {
    let kids: Vector<Rc<Value>> = value_iter(children).collect();
    let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
    m.insert(strkey("type"), Rc::new(Value::Str("stack".into())));
    m.insert(strkey("dir"), Rc::new(Value::Str(dir.into())));
    m.insert(strkey("children"), Rc::new(Value::Array(kids)));
    Rc::new(Value::Map(m))
}

fn widget_constrained(kind: &str, n: i64, m_: i64, child: Rc<Value>) -> Rc<Value> {
    let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
    m.insert(strkey("type"), Rc::new(Value::Str("constrained".into())));
    m.insert(strkey("kind"), Rc::new(Value::Str(kind.into())));
    m.insert(strkey("n"), Rc::new(Value::Int(n)));
    m.insert(strkey("m"), Rc::new(Value::Int(m_)));
    m.insert(strkey("child"), child);
    Rc::new(Value::Map(m))
}
