use std::rc::Rc;

use im::{HashMap as ImHashMap, Vector};
use rizz::runtime::{RuntimeError, Value};
use rizz_ui::styling::normalize_style_value;

use super::super::helpers::{Builtins, as_int, as_str, unit};
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
    (w-vstack [(w-min-cells 1 (w-editor-tree ()))
               (w-cells 1 (w-minibuffer))]))
  (set-frame _frame)
  (set-frame ())   ; clear"#,
    );

    b.be_doc(
        "w-text",
        2,
        |args, _| {
            let text = as_str(&args[0], "w-text")?;
            let style_val = with_editor_mut(|st| {
                let theme = st.theme().borrow();
                normalize_style_value(&args[1], &theme)
            })?;
            let mut span: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            span.insert(strkey("text"), Rc::new(Value::Str(text)));
            if !style_val.is_unit() {
                span.insert(strkey("style"), style_val);
            }
            Ok(Rc::new(Value::Map(span)))
        },
        r#"(w-text/2)
alias of (w-span ...): identical shape and semantics. by convention use w-text
when emitting a standalone span widget and w-span when embedding inside
(w-line [...]). see (show w-span) for the full style grammar.
example:
  (w-text "★" ())
  (w-text " mode " "vague.mode.normal")"#,
    );

    b.be_doc(
        "w-line",
        1,
        |args, _| {
            let spans: Vec<Rc<Value>> = value_iter(&args[0]).collect();
            Ok(widget_line(spans))
        },
        r#"(w-line/1)
build a single-row line widget from a sequence of spans. returns
{"type": "line", "spans": [...] "align"?: "left"|"center"|"right"}.
spans is an array of span maps — typically results of (w-span ...) or
(w-text ...). pair with (w-right-align ...) / (w-center-align ...) to control
horizontal alignment within the allocated rect.
example:
  (w-line [(w-text "left" ())
           (w-text " · " "vague.muted")
           (w-text "right" 'header)])"#,
    );

    b.be_doc(
        "w-right-align",
        1,
        |args, _| Ok(widget_set_align(args[0].clone(), "right")),
        r#"(w-right-align/1)
set "align" to "right" on a widget map (e.g. one returned by (w-line ...)).
non-map widgets pass through unchanged. alignment only takes effect on widgets
that respect it (currently w-line).
example:
  (w-right-align (w-line [(w-text "10:42" 'header)]))"#,
    );

    b.be_doc(
        "w-center-align",
        1,
        |args, _| Ok(widget_set_align(args[0].clone(), "center")),
        r#"(w-center-align/1)
set "align" to "center" on a widget map (e.g. one returned by (w-line ...)).
non-map widgets pass through unchanged.
example:
  (w-center-align (w-line [(w-text "title" 'header)]))"#,
    );

    b.be_doc(
        "w-vstack",
        1,
        |args, _| Ok(widget_stack("vertical", &args[0])),
        r#"(w-vstack/1)
build a vertical stack widget. children are laid out top-to-bottom and honour
their outer constraint (w-cells/w-min-cells/w-fill/w-frac); unconstrained
children default to Min(1).
children is an array of widgets.
example:
  (w-vstack
    [(w-min-cells 1 (w-editor-tree ()))
     (w-cells 1 (_status-line))
     (w-cells 1 (w-minibuffer))])"#,
    );

    b.be_doc(
        "w-hstack",
        1,
        |args, _| Ok(widget_stack("horizontal", &args[0])),
        r#"(w-hstack/1)
build a horizontal stack widget. children are laid out left-to-right and honour
their outer constraint (w-cells/w-min-cells/w-fill/w-frac); unconstrained
children default to Min(1).
children is an array of widgets.
example:
  (w-hstack
    [(w-min-cells 1 (w-line [(w-text "left" ())]))
     (w-fill 1 (w-right-align (w-line [(w-text "right" ())])))])"#,
    );

    b.be_doc(
        "w-cells",
        2,
        |args, _| {
            let n = as_int(&args[0], "w-cells")?.max(0).min(u16::MAX as i64);
            Ok(widget_constrained("cells", n, 1, args[1].clone()))
        },
        r#"(w-cells/2)
wrap child with a fixed-length constraint of N cells (ratatui Constraint::Length).
N is clamped to [0, u16::MAX]. the constraint only matters when child sits
inside an (w-vstack ...) or (w-hstack ...); outside a stack it is ignored.
example:
  (w-vstack [(w-cells 1 (_status-line))
             (w-min-cells 1 (w-editor-tree ()))])"#,
    );
    b.be_doc(
        "w-min-cells",
        2,
        |args, _| {
            let n = as_int(&args[0], "w-min-cells")?.max(0).min(u16::MAX as i64);
            Ok(widget_constrained("min", n, 1, args[1].clone()))
        },
        r#"(w-min-cells/2)
wrap child with a minimum-length constraint of N cells (ratatui Constraint::Min).
N is clamped to [0, u16::MAX]. use this for a region that should grow to fill
leftover space after fixed-size siblings claim theirs.
example:
  (w-vstack [(w-min-cells 1 (w-editor-tree ()))
             (w-cells 1 (w-minibuffer))])"#,
    );
    b.be_doc(
        "w-fill",
        2,
        |args, _| {
            let n = as_int(&args[0], "w-fill")?.max(0).min(u16::MAX as i64);
            Ok(widget_constrained("fill", n, 1, args[1].clone()))
        },
        r#"(w-fill/2)
wrap child with a fill constraint of weight N (ratatui Constraint::Fill). when
several fill children sit in the same stack, the remaining space is split
proportionally to their weights. N is clamped to [0, u16::MAX].
example:
  (w-hstack [(w-fill 1 left)
             (w-fill 2 right-twice-as-wide)])"#,
    );
    b.be_doc(
        "w-frac",
        3,
        |args, _| {
            let n = as_int(&args[0], "w-frac")?.max(0).min(u16::MAX as i64);
            let m = as_int(&args[1], "w-frac")?.max(1).min(u16::MAX as i64);
            Ok(widget_constrained("frac", n, m, args[2].clone()))
        },
        r#"(w-frac/3)
wrap child with a ratio constraint of N/M (ratatui Constraint::Ratio). takes
N/M of the parent stack's space along the stack axis. N is clamped to
[0, u16::MAX], M to [1, u16::MAX].
example:
  (w-hstack [(w-frac 1 3 left)
             (w-frac 2 3 right)])"#,
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
        "w-editor-tree",
        1,
        |args, _| {
            let props = match &*args[0] {
                Value::Map(m) => m.clone(),
                Value::Unit => ImHashMap::new(),
                _ => {
                    return Err(RuntimeError::type_mismatch(
                        "w-editor-tree.props",
                        "map | ()",
                        &args[0],
                    ));
                }
            };
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(strkey("type"), Rc::new(Value::Str("editor-tree".into())));
            if let Some(g) = props.get(&strkey("gutter")) {
                m.insert(strkey("gutter"), g.clone());
            }
            if let Some(w) = props.get(&strkey("gutter-width")) {
                m.insert(strkey("gutter-width"), w.clone());
            }
            Ok(Rc::new(Value::Map(m)))
        },
        r#"(w-editor-tree/1)
the leaf widget that the renderer expands into the current window tree
(with all open splits and their buffers). props is a map (or ()) with optional
keys:
  "gutter":       fn called once per visible row to render the gutter. it
                  receives the file row number (int) or () for rows past EOF,
                  and must return a widget — typically (w-text ...).
  "gutter-width": int columns reserved for the gutter (default 0). when the
                  gutter fn is set, give this enough width for the longest
                  row label you produce.
example:
  (fn _gutter (n)
    (if (= n ())
        (w-text "     " "vague.gutter")
        (w-text (str-join [" " (to-str n) " "] "") "vague.gutter")))
  (w-editor-tree {"gutter": _gutter "gutter-width": 5})"#,
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
  (w-vstack [(w-min-cells 1 (w-editor-tree ()))
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
a widget that renders a single editor buffer into its allocated rect. declared
arity is 0 but it accepts an optional bufno (int >= 0). when bufno is omitted,
the renderer fills it with the enclosing popup's backing buffer — so inside
(popup-open ...) you usually want the no-arg form.
example:
  (w-buffer-view)      ; defer to the popup's buffer
  (w-buffer-view 2)    ; render buffer 2 explicitly"#,
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
