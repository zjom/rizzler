//! Styling primitives shared by the renderer and the lisp surface.
//!
//! - [`Style`] / [`Color`] are the renderer-agnostic style representation.
//! - [`Theme`] holds named [`Style`]s registered from lisp (`face-define`).
//! - `*_from_value` helpers convert rizz [`Value`]s into a `Style`/`Color`
//!   so any builtin or render path can accept the lisp shapes uniformly.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::str::FromStr;

use rizz::runtime::{RuntimeError, Value};

pub type Color = ratatui::style::Color;

/// A *partial* style spec: every attribute is optional, so `None` means
/// "transparent — inherit from the parent face / the layer underneath".
/// `Some(false)` for a bool means "explicitly off" — distinguishable from
/// `None` for inheritance purposes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub reverse: Option<bool>,
    /// Names of faces this style inherits from, in priority order: earlier
    /// names win over later. Only set on faces in the [`Theme`].
    pub inherit: Vec<Rc<str>>,
}

impl Style {
    /// Layer `over` on top of `self`: any attribute set on `over` (`Some(_)`)
    /// wins; attributes left `None` on `over` preserve `self`. Used by both
    /// inheritance resolution and decorator overlays.
    pub fn patch(mut self, over: &Style) -> Self {
        if over.fg.is_some() {
            self.fg = over.fg;
        }
        if over.bg.is_some() {
            self.bg = over.bg;
        }
        if over.bold.is_some() {
            self.bold = over.bold;
        }
        if over.italic.is_some() {
            self.italic = over.italic;
        }
        if over.underline.is_some() {
            self.underline = over.underline;
        }
        if over.reverse.is_some() {
            self.reverse = over.reverse;
        }
        self
    }
}

/// Named style table. Mutated through `face-define`; read by the precompute
/// pass when resolving ident-style references.
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

    /// Resolve a face into a flattened style: walks its `inherit` chain,
    /// layering each parent under the face's own attributes.
    pub fn resolve(&self, name: &str) -> Option<Style> {
        self.resolve_at(name, 32)
    }

    fn resolve_at(&self, name: &str, depth: usize) -> Option<Style> {
        if depth == 0 {
            return None;
        }
        let face = self.faces.get(name)?;
        let mut base = Style::default();
        for parent in face.inherit.iter().rev() {
            if let Some(p) = self.resolve_at(parent, depth - 1) {
                base = base.patch(&p);
            }
        }
        let mut own = face.clone();
        own.inherit.clear();
        Some(base.patch(&own))
    }
}

/// Wrapper that hands the runtime a stable interior-mutable handle. `State`
/// owns one of these; lisp builtins and the render-time snapshot share it.
pub type ThemeCell = RefCell<Theme>;

/// Convert a lisp value into a [`Style`]. Recognized shapes:
///
/// * `'face-name` — look up `face-name` in `theme`. Returns `Style::default()`
///   if unknown.
/// * `{...}` — inline map; see module docs for keys.
/// * `()` — `Style::default()`.
pub fn style_from_value(v: &Rc<Value>, theme: &Theme) -> Result<Style, RuntimeError> {
    match &**v {
        Value::Unit => Ok(Style::default()),
        Value::Ident(s) => Ok(theme.resolve(s).unwrap_or_default()),
        Value::Str(s) => Ok(theme.resolve(s).unwrap_or_default()),
        Value::Map(m) => {
            let mut style = Style::default();
            for (k, val) in m.iter() {
                let key = key_str(k)?;
                match key.as_ref() {
                    "fg" => style.fg = color_from_value(val)?,
                    "bg" => style.bg = color_from_value(val)?,
                    "bold" => style.bold = Some(val.is_truthy()),
                    "italic" => style.italic = Some(val.is_truthy()),
                    "underline" => style.underline = Some(val.is_truthy()),
                    "reverse" => style.reverse = Some(val.is_truthy()),
                    "inherit" => style.inherit = inherit_from_value(val)?,
                    other => {
                        return Err(RuntimeError::TypeMismatch {
                            name: "style".into(),
                            expected: "fg|bg|bold|italic|underline|reverse|inherit".into(),
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

fn inherit_from_value(v: &Rc<Value>) -> Result<Vec<Rc<str>>, RuntimeError> {
    match &**v {
        Value::Unit => Ok(Vec::new()),
        Value::Ident(s) | Value::Str(s) => Ok(vec![s.clone()]),
        Value::Array(xs) => xs
            .iter()
            .map(|x| match &**x {
                Value::Ident(s) | Value::Str(s) => Ok(s.clone()),
                _ => Err(RuntimeError::type_mismatch("inherit-name", "ident|str", x)),
            })
            .collect(),
        _ => Err(RuntimeError::type_mismatch("inherit", "ident|str|array", v)),
    }
}

/// Convert a lisp value into an optional [`Color`]. `()` yields `None`.
pub fn color_from_value(v: &Rc<Value>) -> Result<Option<Color>, RuntimeError> {
    match &**v {
        Value::Unit => Ok(None),
        Value::Ident(s) | Value::Str(s) => {
            Color::from_str(s.as_ref())
                .map(Some)
                .map_err(|_| RuntimeError::TypeMismatch {
                    name: "color".into(),
                    expected: "known color name".into(),
                    got: s.as_ref().into(),
                })
        }
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
    if let Some(b) = style.bold {
        m.insert(key("bold"), Rc::new(Value::Int(b as i64)));
    }
    if let Some(b) = style.italic {
        m.insert(key("italic"), Rc::new(Value::Int(b as i64)));
    }
    if let Some(b) = style.underline {
        m.insert(key("underline"), Rc::new(Value::Int(b as i64)));
    }
    if let Some(b) = style.reverse {
        m.insert(key("reverse"), Rc::new(Value::Int(b as i64)));
    }
    if !style.inherit.is_empty() {
        let arr: im::Vector<Rc<Value>> = style
            .inherit
            .iter()
            .map(|s| Rc::new(Value::Str(s.clone())))
            .collect();
        m.insert(key("inherit"), Rc::new(Value::Array(arr)));
    }
    Rc::new(Value::Map(m))
}

fn color_to_value(c: &Color) -> Rc<Value> {
    use im::HashMap as ImHashMap;
    match c {
        Color::Indexed(i) => Rc::new(Value::Int(*i as i64)),
        Color::Rgb(r, g, b) => {
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            m.insert(key("type"), Rc::new(Value::Str("rgb".into())));
            m.insert(key("r"), Rc::new(Value::Int(*r as i64)));
            m.insert(key("g"), Rc::new(Value::Int(*g as i64)));
            m.insert(key("b"), Rc::new(Value::Int(*b as i64)));
            Rc::new(Value::Map(m))
        }
        other => Rc::new(Value::Str(other.to_string().into())),
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
/// [`style_to_value`] so every leaf becomes a string or int.
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

/// Render a lisp value as a list of styled ratatui spans.
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

/// Convert into ratatui's runtime style type.
pub fn style_to_ratatui(style: &Style) -> ratatui::style::Style {
    use ratatui::style::Modifier;

    let mut s = ratatui::style::Style::default();
    if let Some(c) = &style.fg {
        s = s.fg(*c);
    }
    if let Some(c) = &style.bg {
        s = s.bg(*c);
    }
    let mut m = Modifier::empty();
    if style.bold == Some(true) {
        m |= Modifier::BOLD;
    }
    if style.italic == Some(true) {
        m |= Modifier::ITALIC;
    }
    if style.underline == Some(true) {
        m |= Modifier::UNDERLINED;
    }
    if style.reverse == Some(true) {
        m |= Modifier::REVERSED;
    }
    if !m.is_empty() {
        s = s.add_modifier(m);
    }
    s
}

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
        assert_eq!(s.fg, Some(Color::Red));
        assert_eq!(s.bold, Some(true));
        assert_eq!(s.italic, None);
    }

    #[test]
    fn style_from_ident_resolves_face() {
        let mut theme = Theme::new();
        theme.insert(
            "header".into(),
            Style {
                fg: Some(Color::Cyan),
                bold: Some(true),
                ..Default::default()
            },
        );
        let v = run("'header");
        let s = style_from_value(&v, &theme).unwrap();
        assert_eq!(s.fg, Some(Color::Cyan));
        assert_eq!(s.bold, Some(true));
    }

    #[test]
    fn color_from_indexed_int() {
        let v = run("42");
        let c = color_from_value(&v).unwrap();
        assert_eq!(c, Some(Color::Indexed(42)));
    }

    #[test]
    fn color_from_rgb_via_builtin_shape() {
        let v = rgb_value(60, 90, 130);
        let c = color_from_value(&v).unwrap();
        assert_eq!(c, Some(Color::Rgb(60, 90, 130)));
    }

    #[test]
    fn style_to_value_round_trips_basic() {
        let s = Style {
            fg: Some(Color::Blue),
            bold: Some(true),
            ..Default::default()
        };
        let v = style_to_value(&s);
        let theme = Theme::new();
        let back = style_from_value(&v, &theme).unwrap();
        assert_eq!(back, s);
    }
}
