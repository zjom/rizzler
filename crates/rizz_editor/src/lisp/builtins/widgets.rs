//! Widget constructor builtins (`w-*`, `placement-*`) plus `set-frame` /
//! `set-gutter` for installing render callbacks.

use std::rc::Rc;

use im::{HashMap as ImHashMap, Vector};
use rizz::runtime::{RuntimeError, Value};
use rizz_ui::render::GutterWidth;
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
        r#"(w-span TEXT [STYLE])

Returns widget: a styled span, the map {"text": TEXT, "style"?: STYLE}.
Usable both as a top-level widget (the parser promotes it to a single
line) and as an element of (w-line [...]).

TEXT  — str: the span's text.
STYLE — style: optional. One of:
          ()                       no styling
          face                     a face name resolved against the theme
          {"fg": color "bold": 1 ...}
                                   inline style; keys fg, bg, bold,
                                   italic, underline, reverse, inherit

Example:
  (w-span "hello" 'header)
  (w-span "x" {"fg": 'red "bold": 1})
  (w-span "plain" ())
See also: (w-line SPANS), (face-define NAME STYLE)."#,
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
        r#"(set-frame FN)

Installs the per-frame render callback. FN is called once per frame and
must return the widget tree to draw.

FN — fn: takes no arguments, returns a widget. Pass () to clear the
     callback and revert to the default empty layout.

Example:
  (fn _frame ()
    (w-vstack [(w-size 'min   1 (w-editor-tree))
               (w-size 'cells 1 (w-minibuffer))]))
  (set-frame _frame)
  (set-frame ())   ;; clear
See also: (get-frame), (set-gutter FN WIDTH), (w-editor-tree)."#,
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
        r#"(get-frame)

Returns fn: the per-frame render callback installed by (set-frame), or
() if none is set.
See also: (set-frame FN)."#,
    );

    b.be_doc(
        "w-line",
        1,
        |args, _| {
            let spans: Vec<Rc<Value>> = value_iter(&args[0]).collect();
            let line = widget_line(spans);
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
        r#"(w-line SPANS [ALIGN])

Returns widget: a single-row line built from a sequence of spans.

SPANS — array of widget: span maps, typically results of (w-span ...).
ALIGN — ident: optional. 'left | 'center | 'right (default 'left).

Example:
  (w-line [(w-span "left" ())
           (w-span " · " "vague.muted")
           (w-span "right" 'header)])
  (w-line [(w-span "10:42" 'header)] 'right)
See also: (w-span TEXT [STYLE]), (w-vstack CHILDREN)."#,
    );

    b.be_doc(
        "w-vstack",
        1,
        |args, _| Ok(widget_stack("vertical", &args[0])),
        r#"(w-vstack CHILDREN)

Returns widget: a vertical stack. Children are laid out top-to-bottom
and honor their outer constraint (see (w-size ...)); unconstrained
children default to Min(1).

CHILDREN — array of widget.

Example:
  (w-vstack
    [(w-size 'min   1 (w-editor-tree))
     (w-size 'cells 1 (_status-line))
     (w-size 'cells 1 (w-minibuffer))])
See also: (w-hstack CHILDREN), (w-size KIND N CHILD)."#,
    );

    b.be_doc(
        "w-hstack",
        1,
        |args, _| Ok(widget_stack("horizontal", &args[0])),
        r#"(w-hstack CHILDREN)

Returns widget: a horizontal stack. Children are laid out left-to-right
and honor their outer constraint (see (w-size ...)); unconstrained
children default to Min(1).

CHILDREN — array of widget.

Example:
  (w-hstack
    [(w-size 'min  1 (w-line [(w-span "left" ())]))
     (w-size 'fill 1 (w-line [(w-span "right" ())] 'right))])
See also: (w-vstack CHILDREN), (w-size KIND N CHILD)."#,
    );

    b.be_doc(
        "w-size",
        3,
        |args, _| {
            let kind = as_ident_or_str(&args[0], "w-size.kind")?;
            let kind_str = kind.as_ref();
            // 'frac takes 4 args: (w-size 'frac n m child). Everything else
            // is 3: (w-size kind n child).
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
        r#"(w-size KIND N CHILD)
(w-size 'frac N M CHILD)   ;; 'frac takes a denominator

Returns widget: CHILD wrapped with a layout constraint. Constraints only
matter inside (w-vstack ...) / (w-hstack ...); outside a stack they are
ignored.

KIND  — ident: the constraint flavour:
          'cells — fixed length of N cells
          'min   — at least N cells, grows to fill leftover space
          'fill  — weight-N share of the remaining space
          'frac  — exactly N/M of the parent stack's space; takes the
                   extra denominator arg
N     — int: clamped to [0, 65535].
M     — int: the 'frac denominator, clamped to [1, 65535].
CHILD — widget: the wrapped widget.

Errors when KIND is none of 'cells 'min 'fill 'frac.

Example:
  (w-vstack [(w-size 'cells 1 (_status-line))
             (w-size 'min   1 (w-editor-tree))])
  (w-hstack [(w-size 'frac 1 3 left)
             (w-size 'frac 2 3 right)])
See also: (w-vstack CHILDREN), (w-hstack CHILDREN)."#,
    );

    b.be_doc(
        "w-block",
        2,
        |args, _| {
            let props = match &*args[0] {
                Value::Map(m) => m.clone(),
                Value::Unit => ImHashMap::new(),
                _ => {
                    return Err(RuntimeError::type_mismatch(
                        "w-block.props",
                        "map | ()",
                        &args[0],
                    ));
                }
            };
            let child = args[1].clone();
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
        r#"(w-block PROPS CHILD)

Returns widget: CHILD wrapped in a bordered / titled box. PROPS comes
first, CHILD second — the options-then-content shape matches
(w-overlay PLACEMENT CHILD).

PROPS — map | (): optional keys (unrecognized keys are dropped):
          "border":      "none" | "plain" | "rounded" | "double" |
                         "thick" (default "plain")
          "title":       str shown in the top border
          "face":        face for the content area
          "border-face": face for the border itself
          "title-face":  face for the title text
CHILD — widget: the boxed widget.

Example:
  (w-block {"border":      "rounded"
            "title":       " Status "
            "border-face": "vague.muted"}
           (w-line [(w-span "hello" ())]))
See also: (w-overlay PLACEMENT CHILD), (w-popup-self)."#,
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
        r#"(w-overlay PLACEMENT CHILD)

Returns widget: CHILD painted on top of the rest of the frame at
PLACEMENT. An overlay has no backing buffer and never receives keys — it
is pure decoration: floating status indicators, toasts, hover hints, any
chrome that should sit above the editor without stealing focus.

PLACEMENT — placement: from a (placement-*) constructor, the same shape
            as the "placement" key in (popup-show ...). Resolved against
            the rect the overlay sits inside; when the overlay is a child
            of a (w-vstack)/(w-hstack) it floats over the *whole* stack
            (the layout pretends it isn't there). Nest it deeper to scope
            it.
CHILD     — widget: any widget — (w-block), (w-vstack), (w-line),
            (w-buffer-view BUFNO), … are all available.

Overlays are transparent by default: only the cells CHILD actually draws
are touched, and what was painted underneath stays visible everywhere
else. Wrap CHILD in (w-block {"face": "popup.default"} …) for an opaque
popup-style backdrop.

Example:
  (w-vstack
    [(w-size 'min 1 (w-editor-tree))
     (w-size 'cells 1 (_status-line))
     ;; floats over the whole vstack, doesn't steal a row from anyone,
     ;; doesn't erase what's behind it.
     (w-overlay (placement-anchored 'top 1)
                (w-line [(w-span " saving… " 'header)] 'right))])
See also: (popup-show NAME WIDGET), (placement-anchored SIDE SIZE)."#,
    );

    b.be_doc(
        "w-editor-tree",
        0,
        |_, _| {
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(strkey("type"), Rc::new(Value::Str("editor-tree".into())));
            Ok(Rc::new(Value::Map(m)))
        },
        r#"(w-editor-tree)

Returns widget: the leaf the renderer expands into the current window
tree — all open splits and their buffers. The gutter is configured
separately via (set-gutter FN WIDTH), since it's a per-buffer concern,
not a per-layout one.

Example:
  (w-editor-tree)
See also: (set-frame FN), (set-gutter FN WIDTH), (w-minibuffer)."#,
    );

    b.be_doc(
        "set-gutter",
        2,
        |args, _| {
            let f = args[0].clone();
            let width = parse_gutter_width(&args[1])?;
            let fn_opt = if f.is_unit() { None } else { Some(f) };
            with_editor_mut(|st| st.set_gutter(fn_opt, width));
            Ok(unit())
        },
        r#"(set-gutter FN WIDTH)

Installs the per-row gutter callback used by every file buffer.

FN    — fn: called once per visible row with the file row number (int)
        or () for rows past EOF; must return a widget, typically a
        (w-line ...). Pass () to disable the gutter entirely.
WIDTH — int | ident: columns reserved on the left of the buffer view:
          'fit | ()  — size to the widest row FN returns this frame
                       (default)
          int        — reserve exactly that many columns (0 disables)

Example:
  (fn _gutter (n)
    (if (= n ())
        (w-line [(w-span "     " "vague.gutter")])
        (w-line [(w-span (str-join [" " (to-str n) " "] "")
                         "vague.gutter")])))
  (set-gutter _gutter 'fit)   ;; shrinks/grows with content
  (set-gutter _gutter 5)      ;; fixed 5-column gutter
  (set-gutter () 0)           ;; disable
See also: (set-frame FN), (w-editor-tree)."#,
    );

    b.be_doc(
        "w-minibuffer",
        0,
        |_, _| {
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(strkey("type"), Rc::new(Value::Str("minibuffer".into())));
            Ok(Rc::new(Value::Map(m)))
        },
        r#"(w-minibuffer)

Returns widget: the single-row minibuffer leaf. Place it in your frame
tree — typically the bottom row of an outer (w-vstack ...) — so command-
mode input has somewhere to render.

Example:
  (w-vstack [(w-size 'min   1 (w-editor-tree))
             (w-size 'cells 1 (w-minibuffer))])
See also: (w-editor-tree), (minibuffer-bufno)."#,
    );

    b.be_doc(
        "w-empty",
        0,
        |_, _| {
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(strkey("type"), Rc::new(Value::Str("empty".into())));
            Ok(Rc::new(Value::Map(m)))
        },
        r#"(w-empty)

Returns widget: a widget that draws nothing and takes zero layout cells.
Use it as the "off" branch of a conditional in a stack — the slot
vanishes instead of leaving an empty row behind.

Example:
  (w-vstack
    [(w-size 'min 1 (w-editor-tree))
     (w-size 'cells 1 (w-minibuffer))
     (if show-help (_help-banner) (w-empty))])  ;; row gone when off
See also: (w-vstack CHILDREN)."#,
    );

    b.be_doc(
        "w-buffer-view",
        1,
        |args, _| {
            let bufno = as_int(&args[0], "w-buffer-view.bufno")?.max(0);
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(strkey("type"), Rc::new(Value::Str("buffer-view".into())));
            m.insert(strkey("bufno"), Rc::new(Value::Int(bufno)));
            Ok(Rc::new(Value::Map(m)))
        },
        r#"(w-buffer-view BUFNO)

Returns widget: a view that renders buffer BUFNO into its allocated rect.

BUFNO — bufno: from (buffer-no), (popup-bufno NAME), or (minibuffer-bufno).

For the popup-specific "render the popup's own backing buffer" case,
prefer (w-popup-self) — it names that pattern explicitly and needs no
BUFNO argument.

Example:
  (w-buffer-view (buffer-no))                  ;; the focused buffer
  (w-buffer-view (popup-bufno 'messages))   ;; a named popup's buf
  (w-block {"face": "popup.default"}
           (w-buffer-view (minibuffer-bufno)))
See also: (w-popup-self), (buffer-no), (popup-bufno NAME)."#,
    );

    b.be_doc(
        "w-popup-self",
        0,
        |_, _| {
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(strkey("type"), Rc::new(Value::Str("buffer-view".into())));
            Ok(Rc::new(Value::Map(m)))
        },
        r#"(w-popup-self)

Returns widget: inside a popup widget tree (the one passed to
(popup-show)), a view of the popup's own backing buffer — the buf
holding its text content. No BUFNO argument: the renderer fills it in
from the enclosing popup, which is exactly why it's needed — the bufno
isn't known until (popup-show) creates the panel.

A no-op outside a popup; there's no enclosing buffer in the main frame
fn, so use (w-buffer-view BUFNO) there.

Example:
  (popup-show 'messages
    (w-block {"face": "popup.default"} (w-popup-self))
    {"text": "hi"})
See also: (popup-show NAME WIDGET), (w-buffer-view BUFNO)."#,
    );

    b.be_doc(
        "placement-centered",
        2,
        |args, _| {
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(strkey("kind"), Rc::new(Value::Str("centered".into())));
            m.insert(strkey("w"), args[0].clone());
            m.insert(strkey("h"), args[1].clone());
            Ok(Rc::new(Value::Map(m)))
        },
        r#"(placement-centered W H)

Returns placement: a centered placement, for (popup-show)'s "placement"
option or (w-overlay PLACEMENT CHILD).

W, H — each a dim: an int (cells), a float in [0.0, 1.0] (fraction of
       parent), or 'fit (sized to content).

Example:
  (placement-centered 0.6 0.6)
  (placement-centered 40 'fit)
See also: (placement-anchored SIDE SIZE), (placement-at X Y W H),
(placement-at-cursor W H)."#,
    );

    b.be_doc(
        "placement-anchored",
        2,
        |args, _| {
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(strkey("kind"), Rc::new(Value::Str("side".into())));
            m.insert(strkey("side"), args[0].clone());
            m.insert(strkey("size"), args[1].clone());
            Ok(Rc::new(Value::Map(m)))
        },
        r#"(placement-anchored SIDE SIZE)

Returns placement: a placement that pins the overlay to one side of the
parent rect.

SIDE — ident: 'top | 'bottom | 'left | 'right.
SIZE — dim: an int (cells), a float (fraction), or 'fit (size to
       content).

Example:
  (placement-anchored 'bottom 5)
  (placement-anchored 'top 'fit)
See also: (placement-centered W H), (placement-at X Y W H)."#,
    );

    b.be_doc(
        "placement-at",
        4,
        |args, _| {
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(strkey("kind"), Rc::new(Value::Str("at".into())));
            m.insert(strkey("x"), args[0].clone());
            m.insert(strkey("y"), args[1].clone());
            m.insert(strkey("w"), args[2].clone());
            m.insert(strkey("h"), args[3].clone());
            Ok(Rc::new(Value::Map(m)))
        },
        r#"(placement-at X Y W H)

Returns placement: a placement anchored at (X, Y) in the parent rect with
the given width and height.

X, Y — int: cell offsets from the parent's origin.
W, H — dim: same shape as in (placement-centered) — int, float, or 'fit.

Example:
  (placement-at 0 0 40 10)
  (placement-at 10 5 'fit 'fit)
See also: (placement-centered W H), (placement-at-cursor W H)."#,
    );

    b.be_doc(
        "placement-at-cursor",
        2,
        |args, _| {
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(strkey("kind"), Rc::new(Value::Str("at-cursor".into())));
            m.insert(strkey("w"), args[0].clone());
            m.insert(strkey("h"), args[1].clone());
            Ok(Rc::new(Value::Map(m)))
        },
        r#"(placement-at-cursor W H)

Returns placement: a placement anchored next to the focused editor
cursor. The renderer drops the popup one row below the cursor when
there's room and above it otherwise, keeping it inside the focused window
leaf — so the placement stays correct under splits and custom frame
layouts.

W, H — dim: same shape as in (placement-centered):
         int    — absolute cell count
         float  — fraction of the leaf's width / height (0.0..=1.0)
         'fit   — hug content (the popup's text bounds)

Example:
  (placement-at-cursor 'fit 8)      ;; hug width, 8 rows tall
  (placement-at-cursor 'fit 'fit)   ;; hug both axes
See also: (placement-centered W H), (cursor-screen-row)."#,
    );
}

fn strkey(s: &str) -> Rc<Value> {
    Rc::new(Value::Str(s.into()))
}

fn parse_gutter_width(v: &Rc<Value>) -> Result<GutterWidth, RuntimeError> {
    match &**v {
        Value::Unit => Ok(GutterWidth::Fit),
        Value::Ident(s) | Value::Str(s) if s.as_ref() == "fit" => Ok(GutterWidth::Fit),
        Value::Int(n) => {
            let n = (*n).max(0).min(u16::MAX as i64) as u16;
            Ok(GutterWidth::Fixed(n))
        }
        _ => Err(RuntimeError::type_mismatch(
            "set-gutter.width",
            "int | 'fit | ()",
            v,
        )),
    }
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
