//! Styling primitives shared by the renderer and the lisp surface.
//!
//! - [`Style`] / [`Color`] are the renderer-agnostic style representation.
//! - [`Theme`] holds named [`Style`]s registered from lisp (`face-define`).
//! - `*_from_value` helpers convert rizz [`Value`]s into a `Style`/`Color`
//!   so any builtin or render path can accept the lisp shapes uniformly.
//!
//! Conventions for style maps from lisp:
//!
//! * Map keys must be strings — rizz's parser doesn't terminate idents at `:`,
//!   so `{fg: ...}` and `{'fg: ...}` parse incorrectly. Use `{"fg": ...}`.
//! * Recognized keys: `fg`, `bg`, `bold`, `italic`, `underline`, `reverse`.
//! * Color values: a named ident (`'red`, `'dark-gray`), an int (xterm
//!   indexed color), or the tagged map produced by `(rgb r g b)`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use rizz::runtime::{RuntimeError, Value};

// ---------------------------------------------------------------------------
// Style
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
}

impl Style {
    /// Layer `over` on top of `self`: each field of `over` wins if set; `bool`
    /// modifiers are OR-ed so a base style's bold survives a non-bold overlay.
    pub fn patch(mut self, over: &Style) -> Self {
        if over.fg.is_some() {
            self.fg = over.fg.clone();
        }
        if over.bg.is_some() {
            self.bg = over.bg.clone();
        }
        self.bold |= over.bold;
        self.italic |= over.italic;
        self.underline |= over.underline;
        self.reverse |= over.reverse;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Color {
    Named(NamedColor),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamedColor {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    Gray,
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    White,
    Reset,
}

impl NamedColor {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "black" => Self::Black,
            "red" => Self::Red,
            "green" => Self::Green,
            "yellow" => Self::Yellow,
            "blue" => Self::Blue,
            "magenta" => Self::Magenta,
            "cyan" => Self::Cyan,
            "gray" | "grey" => Self::Gray,
            "dark-gray" | "dark-grey" => Self::DarkGray,
            "light-red" => Self::LightRed,
            "light-green" => Self::LightGreen,
            "light-yellow" => Self::LightYellow,
            "light-blue" => Self::LightBlue,
            "light-magenta" => Self::LightMagenta,
            "light-cyan" => Self::LightCyan,
            "white" => Self::White,
            "reset" | "default" => Self::Reset,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

/// Named style table. Mutated through `face-define`; read by the precompute
/// pass when resolving ident-style references. The renderer sees a cloned
/// snapshot so a `face-define` from one slot's callback can't shift styles
/// mid-frame.
#[derive(Clone, Debug, Default)]
pub struct Theme {
    faces: HashMap<Rc<str>, Style>,
}

impl Theme {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, name: Rc<str>, style: Style) {
        self.faces.insert(name, style);
    }

    pub fn lookup(&self, name: &str) -> Option<&Style> {
        self.faces.get(name)
    }
}

/// Wrapper that hands the runtime a stable interior-mutable handle. `State`
/// owns one of these; lisp builtins and the render-time snapshot share it.
pub type ThemeCell = RefCell<Theme>;

// ---------------------------------------------------------------------------
// Value -> Style / Color
// ---------------------------------------------------------------------------

/// Convert a lisp value into a [`Style`]. Recognized shapes:
///
/// * `'face-name` — look up `face-name` in `theme`. Returns `Style::default()`
///   if unknown.
/// * `{...}` — inline map; see module docs for keys.
/// * `()` — `Style::default()`.
pub fn style_from_value(v: &Rc<Value>, theme: &Theme) -> Result<Style, RuntimeError> {
    match &**v {
        Value::Unit => Ok(Style::default()),
        Value::Ident(s) => Ok(theme.lookup(s).cloned().unwrap_or_default()),
        Value::Str(s) => Ok(theme.lookup(s).cloned().unwrap_or_default()),
        Value::Map(m) => {
            let mut style = Style::default();
            for (k, val) in m.iter() {
                let key = key_str(k)?;
                match key.as_ref() {
                    "fg" => style.fg = color_from_value(val)?,
                    "bg" => style.bg = color_from_value(val)?,
                    "bold" => style.bold = val.is_truthy(),
                    "italic" => style.italic = val.is_truthy(),
                    "underline" => style.underline = val.is_truthy(),
                    "reverse" => style.reverse = val.is_truthy(),
                    other => {
                        return Err(RuntimeError::TypeMismatch {
                            name: "style".into(),
                            expected: "fg|bg|bold|italic|underline|reverse".into(),
                            got: other.into(),
                        });
                    }
                }
            }
            Ok(style)
        }
        _ => Err(RuntimeError::type_mismatch("style", "ident|map|()", v)),
    }
}

/// Convert a lisp value into an optional [`Color`]. `()` yields `None` (i.e.
/// no color set). Accepts: named idents/strings, an `Int` (xterm indexed
/// color, 0..=255), or the tagged map `{'type: 'rgb 'r: N 'g: N 'b: N}` that
/// the `(rgb ...)` builtin produces.
pub fn color_from_value(v: &Rc<Value>) -> Result<Option<Color>, RuntimeError> {
    match &**v {
        Value::Unit => Ok(None),
        Value::Ident(s) | Value::Str(s) => NamedColor::parse(s)
            .map(|c| Some(Color::Named(c)))
            .ok_or_else(|| RuntimeError::TypeMismatch {
                name: "color".into(),
                expected: "known color name".into(),
                got: s.as_ref().into(),
            }),
        Value::Int(n) => {
            let n = u8::try_from(*n).map_err(|_| RuntimeError::TypeMismatch {
                name: "color".into(),
                expected: "indexed color 0..=255".into(),
                got: n.to_string().into(),
            })?;
            Ok(Some(Color::Indexed(n)))
        }
        Value::Map(m) => {
            let ty = m
                .get(&key("type"))
                .ok_or_else(|| RuntimeError::TypeMismatch {
                    name: "color".into(),
                    expected: "tagged color map (missing \"type\")".into(),
                    got: "map".into(),
                })?;
            let ty_s = key_str(ty)?;
            match ty_s.as_ref() {
                "rgb" => {
                    let r = map_u8(m, "r")?;
                    let g = map_u8(m, "g")?;
                    let b = map_u8(m, "b")?;
                    Ok(Some(Color::Rgb(r, g, b)))
                }
                other => Err(RuntimeError::TypeMismatch {
                    name: "color".into(),
                    expected: "rgb".into(),
                    got: other.into(),
                }),
            }
        }
        _ => Err(RuntimeError::type_mismatch(
            "color",
            "ident|str|int|rgb-map|()",
            v,
        )),
    }
}

/// Convert a [`Style`] back into a lisp map so `(face-of ...)` can return a
/// readable representation.
pub fn style_to_value(style: &Style) -> Rc<Value> {
    use im::HashMap as ImHashMap;

    let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
    if let Some(c) = &style.fg {
        m.insert(key("fg"), color_to_value(c));
    }
    if let Some(c) = &style.bg {
        m.insert(key("bg"), color_to_value(c));
    }
    if style.bold {
        m.insert(key("bold"), Rc::new(Value::Int(1)));
    }
    if style.italic {
        m.insert(key("italic"), Rc::new(Value::Int(1)));
    }
    if style.underline {
        m.insert(key("underline"), Rc::new(Value::Int(1)));
    }
    if style.reverse {
        m.insert(key("reverse"), Rc::new(Value::Int(1)));
    }
    Rc::new(Value::Map(m))
}

fn color_to_value(c: &Color) -> Rc<Value> {
    use im::HashMap as ImHashMap;
    match c {
        Color::Named(n) => Rc::new(Value::Str(named_to_str(*n).into())),
        Color::Indexed(i) => Rc::new(Value::Int(*i as i64)),
        Color::Rgb(r, g, b) => {
            // Tagged map with string keys so that re-evaluation by the
            // runtime (post-call) doesn't try to bind 'type, 'r, etc. as
            // identifiers.
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(key("type"), Rc::new(Value::Str("rgb".into())));
            m.insert(key("r"), Rc::new(Value::Int(*r as i64)));
            m.insert(key("g"), Rc::new(Value::Int(*g as i64)));
            m.insert(key("b"), Rc::new(Value::Int(*b as i64)));
            Rc::new(Value::Map(m))
        }
    }
}

fn named_to_str(c: NamedColor) -> &'static str {
    match c {
        NamedColor::Black => "black",
        NamedColor::Red => "red",
        NamedColor::Green => "green",
        NamedColor::Yellow => "yellow",
        NamedColor::Blue => "blue",
        NamedColor::Magenta => "magenta",
        NamedColor::Cyan => "cyan",
        NamedColor::Gray => "gray",
        NamedColor::DarkGray => "dark-gray",
        NamedColor::LightRed => "light-red",
        NamedColor::LightGreen => "light-green",
        NamedColor::LightYellow => "light-yellow",
        NamedColor::LightBlue => "light-blue",
        NamedColor::LightMagenta => "light-magenta",
        NamedColor::LightCyan => "light-cyan",
        NamedColor::White => "white",
        NamedColor::Reset => "reset",
    }
}

fn key(s: &str) -> Rc<Value> {
    Rc::new(Value::Str(s.into()))
}

fn key_str(v: &Rc<Value>) -> Result<Rc<str>, RuntimeError> {
    match &**v {
        Value::Ident(s) | Value::Str(s) => Ok(s.clone()),
        _ => Err(RuntimeError::type_mismatch("style-key", "ident|str", v)),
    }
}

fn map_u8(m: &im::HashMap<Rc<Value>, Rc<Value>>, field: &str) -> Result<u8, RuntimeError> {
    let v = m
        .get(&key(field))
        .ok_or_else(|| RuntimeError::TypeMismatch {
            name: "rgb".into(),
            expected: format!("\"{field}\" field").into(),
            got: "missing".into(),
        })?;
    let n = v
        .as_int()
        .ok_or_else(|| RuntimeError::type_mismatch(&format!("rgb '{field}"), "int 0..=255", v))?;
    u8::try_from(n).map_err(|_| RuntimeError::TypeMismatch {
        name: "rgb".into(),
        expected: "0..=255".into(),
        got: n.to_string().into(),
    })
}

/// Build the tagged-map representation the `(rgb r g b)` builtin returns.
pub fn rgb_value(r: u8, g: u8, b: u8) -> Rc<Value> {
    color_to_value(&Color::Rgb(r, g, b))
}

/// Normalize a user-supplied style expression into a form that survives
/// rizz's post-call re-evaluation: face references collapse to `Value::Str`
/// (the face name), inline maps are routed through [`style_from_value`] and
/// [`style_to_value`] so every leaf becomes a string or int. `Unit` passes
/// through.
pub fn normalize_style_value(v: &Rc<Value>, theme: &Theme) -> Result<Rc<Value>, RuntimeError> {
    match &**v {
        Value::Unit => Ok(v.clone()),
        Value::Ident(s) | Value::Str(s) => Ok(Rc::new(Value::Str(s.clone()))),
        Value::Map(_) => {
            let style = style_from_value(v, theme)?;
            Ok(style_to_value(&style))
        }
        _ => Err(RuntimeError::type_mismatch(
            "style",
            "face name (ident|str), inline style map, or ()",
            v,
        )),
    }
}

// ---------------------------------------------------------------------------
// Value -> ratatui spans
// ---------------------------------------------------------------------------

/// Render a lisp value as a list of styled ratatui spans. Accepted shapes:
///
/// * `Str` — single unstyled span containing the string.
/// * `Map` — single span if it has a `"text"` key, optionally styled by the
///   `"style"` key (face name or inline style map).
/// * `Array` / `Cons` — sequence of any of the above.
/// * `()` — empty span list.
///
/// Returns spans owning their text (`'static` lifetime) so the caller can
/// stash them in a `RenderedFrame` without lifetime juggling.
pub fn spans_from_value(
    v: &Rc<Value>,
    theme: &Theme,
) -> Result<Vec<ratatui::text::Span<'static>>, RuntimeError> {
    let mut out = Vec::new();
    append_spans(v, theme, &mut out)?;
    Ok(out)
}

fn append_spans(
    v: &Rc<Value>,
    theme: &Theme,
    out: &mut Vec<ratatui::text::Span<'static>>,
) -> Result<(), RuntimeError> {
    use ratatui::text::Span;

    match &**v {
        Value::Unit => Ok(()),
        Value::Str(s) | Value::Ident(s) => {
            out.push(Span::raw(s.to_string()));
            Ok(())
        }
        Value::Int(n) => {
            out.push(Span::raw(n.to_string()));
            Ok(())
        }
        Value::Map(_) => {
            let span = span_from_map(v, theme)?;
            out.push(span);
            Ok(())
        }
        Value::Array(xs) => {
            for x in xs.iter() {
                append_spans(x, theme, out)?;
            }
            Ok(())
        }
        Value::Cons { .. } => {
            for x in Value::iter(v) {
                append_spans(&x, theme, out)?;
            }
            Ok(())
        }
        _ => Err(RuntimeError::type_mismatch(
            "span",
            "str|ident|int|map|array|list|()",
            v,
        )),
    }
}

fn span_from_map(
    v: &Rc<Value>,
    theme: &Theme,
) -> Result<ratatui::text::Span<'static>, RuntimeError> {
    use ratatui::text::Span;

    let m = match &**v {
        Value::Map(m) => m,
        _ => unreachable!("span_from_map called on non-map"),
    };
    let text = m
        .get(&key("text"))
        .and_then(|t| t.as_str())
        .ok_or_else(|| RuntimeError::TypeMismatch {
            name: "span".into(),
            expected: "map with \"text\" str field".into(),
            got: Value::type_name(v).into(),
        })?;
    let style = match m.get(&key("style")) {
        Some(s) => style_from_value(s, theme)?,
        None => Style::default(),
    };
    Ok(Span::styled(text.to_string(), style_to_ratatui(&style)))
}

// ---------------------------------------------------------------------------
// Style -> ratatui
// ---------------------------------------------------------------------------

/// Convert into ratatui's runtime style type. Kept in this module so the
/// styling representation has exactly one boundary with ratatui.
pub fn style_to_ratatui(style: &Style) -> ratatui::style::Style {
    use ratatui::style::Modifier;

    let mut s = ratatui::style::Style::default();
    if let Some(c) = &style.fg {
        s = s.fg(color_to_ratatui(c));
    }
    if let Some(c) = &style.bg {
        s = s.bg(color_to_ratatui(c));
    }
    let mut m = Modifier::empty();
    if style.bold {
        m |= Modifier::BOLD;
    }
    if style.italic {
        m |= Modifier::ITALIC;
    }
    if style.underline {
        m |= Modifier::UNDERLINED;
    }
    if style.reverse {
        m |= Modifier::REVERSED;
    }
    if !m.is_empty() {
        s = s.add_modifier(m);
    }
    s
}

fn color_to_ratatui(c: &Color) -> ratatui::style::Color {
    use ratatui::style::Color as RC;
    match c {
        Color::Named(n) => match n {
            NamedColor::Black => RC::Black,
            NamedColor::Red => RC::Red,
            NamedColor::Green => RC::Green,
            NamedColor::Yellow => RC::Yellow,
            NamedColor::Blue => RC::Blue,
            NamedColor::Magenta => RC::Magenta,
            NamedColor::Cyan => RC::Cyan,
            NamedColor::Gray => RC::Gray,
            NamedColor::DarkGray => RC::DarkGray,
            NamedColor::LightRed => RC::LightRed,
            NamedColor::LightGreen => RC::LightGreen,
            NamedColor::LightYellow => RC::LightYellow,
            NamedColor::LightBlue => RC::LightBlue,
            NamedColor::LightMagenta => RC::LightMagenta,
            NamedColor::LightCyan => RC::LightCyan,
            NamedColor::White => RC::White,
            NamedColor::Reset => RC::Reset,
        },
        Color::Indexed(i) => RC::Indexed(*i),
        Color::Rgb(r, g, b) => RC::Rgb(*r, *g, *b),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> Rc<Value> {
        let (v, _) = rizz::parse_and_run(src.as_bytes()).expect("eval failed");
        v
    }

    #[test]
    fn style_from_map_with_string_keys() {
        let v = run(r#"{"fg": 'red "bold": 1}"#);
        let theme = Theme::new();
        let s = style_from_value(&v, &theme).unwrap();
        assert_eq!(s.fg, Some(Color::Named(NamedColor::Red)));
        assert!(s.bold);
        assert!(!s.italic);
    }

    #[test]
    fn style_from_ident_resolves_face() {
        let mut theme = Theme::new();
        theme.insert(
            "header".into(),
            Style {
                fg: Some(Color::Named(NamedColor::Cyan)),
                bold: true,
                ..Default::default()
            },
        );
        let v = run("'header");
        let s = style_from_value(&v, &theme).unwrap();
        assert_eq!(s.fg, Some(Color::Named(NamedColor::Cyan)));
        assert!(s.bold);
    }

    #[test]
    fn unknown_face_yields_default_style() {
        let theme = Theme::new();
        let v = run("'no-such-face");
        let s = style_from_value(&v, &theme).unwrap();
        assert_eq!(s, Style::default());
    }

    #[test]
    fn color_from_indexed_int() {
        let v = run("42");
        let c = color_from_value(&v).unwrap();
        assert_eq!(c, Some(Color::Indexed(42)));
    }

    #[test]
    fn color_from_rgb_via_builtin_shape() {
        // The `(rgb r g b)` builtin returns a tagged map with ident keys;
        // construct an equivalent value directly here (rizz's parser can't
        // express ident-keyed literals).
        let v = rgb_value(60, 90, 130);
        let c = color_from_value(&v).unwrap();
        assert_eq!(c, Some(Color::Rgb(60, 90, 130)));
    }

    #[test]
    fn color_unit_means_none() {
        let v = Rc::new(Value::Unit);
        let c = color_from_value(&v).unwrap();
        assert_eq!(c, None);
    }

    #[test]
    fn style_to_value_round_trips_basic() {
        let s = Style {
            fg: Some(Color::Named(NamedColor::Blue)),
            bold: true,
            ..Default::default()
        };
        let v = style_to_value(&s);
        let theme = Theme::new();
        let back = style_from_value(&v, &theme).unwrap();
        assert_eq!(back, s);
    }
}
