# Documenting lisp builtins

Every native builtin the editor exposes to `init.rz` carries a **doc string** —
the `&'static str` passed as the last argument to `be_doc` / `bi_doc` (see
[`helpers.rs`](../crates/rizz_editor/src/lisp/builtins/mod.rs)). Users read it at
runtime with `(show 'name)`, which prints the string verbatim. The host language
itself documents its special forms (`fn`, `let`, `if`, …) through the same
`(show …)` surface, so a builtin's doc must read like one of those: same shape,
same tone, same notation.

This file is the standard. Every `be_doc` / `bi_doc` doc string follows it. New
builtins use `be_doc` / `bi_doc`, never the doc-less `be` / `bi`.

## The shape

A doc string is **plain monospaced text**, not markdown — it lands in a popup or
a `notify`, unrendered. Wrap prose at ~72 columns. Structure it as a fixed
sequence of sections; only the **signature** and the **summary** are required,
the rest appear when they carry their weight:

```
SIGNATURE                          ← line 1 (one line per overload)
                                   ← blank line
Summary sentence, then semantics.  ← sentence-case prose

PARAMS                             ← when an argument needs explaining
Returns ...                        ← when it returns a value worth typing
Errors ...                         ← when it fails in a notable way

Example:                           ← when a call is non-obvious
  (name ...)

See also: (other), (related).      ← when a neighbour belongs in the reader's head
```

Sections always appear in this order. Drop any that would only state the
obvious — a doc that earns its keep beats a doc that fills a template.

## 1. Signature

The first line is an s-expression naming the function and its parameters with
**UPPERCASE metavariables** — the same convention rizz uses for its special
forms (`(if COND THEN ELSE)`).

| Form              | Meaning                                             |
| ----------------- | --------------------------------------------------- |
| `(name ARG)`      | one required positional argument                    |
| `(name)`          | takes no arguments                                  |
| `(name [ARG])`    | `ARG` is optional                                   |
| `(name ARG...)`   | variadic — zero or more trailing values            |
| `(name A [B])`    | `B` optional after required `A`                     |
| two+ stacked lines | distinct overloads / arities                        |

Name parameters; never use the `/arity` shorthand — `(popup-show NAME WIDGET
[OPTS])` says more than `(popup-show/2 | /3)` and matches the host. A trailing
`;; comment` after a signature line is allowed to disambiguate an overload:

```
(w-size KIND N CHILD)
(w-size 'frac N M CHILD)   ;; 'frac takes a denominator
```

The metavariables you pick reappear, unchanged and still uppercase, in the prose
and the PARAMS block. One name, one referent.

## 2. Summary and semantics

A blank line, then **sentence-case prose** — capitalized, punctuated, present
tense, describing the function from the caller's side. Lead with one sentence
that says what it does or returns; that sentence is what a reader skims. Then add
whatever semantics matter: side effects, ordering, what "in place" means, how it
interacts with the editor's modes/funnel, edge cases.

Sentence case is deliberate. `(show 'fn)` yields *"Creates a closure capturing
the current env…"*; `(show 'quit)` should read the same way, not drop into a
lowercase register. Match the host.

Refer to other builtins by their call form in backtick-free prose: write
`(popup-close)`, `(buffer-no)` — they read as code and double as cross-references.

## 3. Parameters

When an argument's type or accepted values aren't obvious from its name, spell it
out. Use a short block, one parameter per line, metavariable then an em-dash then
`type` then prose:

```
NAME   — ident | str: the panel's key. Reusing a name updates in place.
OPTS   — map: optional. Recognized keys:
           "text":      str  — seed text for the backing buffer
           "placement": placement — see (placement-centered ...)
```

Skip the block entirely for a nullary builtin or a single self-evident argument
(`(insert STR)` needs no PARAMS section). Don't restate the signature as prose.

### Types

Draw types from this vocabulary. The first group is rizz's own value types; the
second is editor-domain aliases — each is really one of the core types, named for
what it carries so the reader knows where it comes from.

**Core value types**

| Type    | Notes                                                              |
| ------- | ----------------------------------------------------------------- |
| `str`   | string                                                            |
| `int`   | 64-bit integer                                                   |
| `float` | floating point                                                   |
| `ident` | a quoted symbol, e.g. `'normal`                                  |
| `array` | `[a b c]`                                                        |
| `map`   | `{"k": v}`                                                       |
| `fn`    | a callable (closure / native fn)                                |
| `unit`  | `()` — also the false / absent value                            |

There is no boolean type. `()` and `0` are **false**; everything else is
**true**. Predicates — builtins whose name ends in `?` — return `1` for true and
`0` for false; say so as *"Returns 1 if … else 0."*

**Editor-domain types** (write these names in PARAMS; define them inline if
exotic)

| Type          | Underlying      | Carries                                                       |
| ------------- | --------------- | ------------------------------------------------------------- |
| `bufno`       | int             | opaque buffer id from `(buffer-no)`, `(popup-bufno …)`, etc.     |
| `mode`        | ident           | `'normal 'insert 'visual 'visual-line 'visual-block 'command` |
| `move-kind`   | ident           | a cursor motion, e.g. `'word-next 'line-start`                |
| `text-object` | ident \| str    | a vim text object, e.g. `'word 'paren 'quote`                 |
| `layer`       | ident \| str    | a keymap layer name                                           |
| `key-seq`     | str             | a key sequence, e.g. `"C-x"`                                  |
| `face`        | ident \| str    | a face name resolved against the theme                       |
| `color`       | ident \| str    | a color name or `(rgb R G B)` value                          |
| `style`       | `()` \| face \| map | no styling, a face, or an inline `{"fg": … "bold": 1}`    |
| `widget`      | map             | a widget tree from a `w-*` constructor                        |
| `placement`   | map             | from a `placement-*` constructor                             |
| `path`        | str             | a filesystem path                                            |
| `register`    | str             | a single-character register name                             |

## 4. Returns

State the return when the builtin produces a value the caller uses, and the type
or shape isn't already plain from the summary. Lead with the type:

```
Returns bufno: the backing buffer's opaque id.
Returns map: {"stdout": str, "stderr": str, "code": int|(), "success?": 1|0}.
```

Omit it for command builtins that return `()` — say so in the summary if it
matters ("…and returns ()"), otherwise let it go. For predicates, fold the
return into the summary rather than writing a separate line.

## 5. Errors

A builtin signals failure by returning a `RuntimeError`. The generic
"wrong-typed argument" error is implied by the PARAMS types and needs no mention.
Document **notable** failure modes — the ones a caller should anticipate:
unknown enum variants, missing external tools, out-of-range ids, I/O. One
sentence, lead with `Errors`:

```
Errors when KIND is not one of 'cells 'min 'fill 'frac.
Errors when `git` or `tree-sitter` is not on $PATH.
```

## 6. Example

Show a real call when usage isn't obvious from the signature — anything with a
map argument, a callback, or a non-trivial value shape earns one. Label it
`Example:` (or `Examples:`), then indent the snippet two spaces. Use `;;`
comments to annotate. Keep it runnable and minimal:

```
Example:
  (popup-show 'help
    (w-block {"border": "rounded" "title": " help "} (w-popup-self))
    {"text": "press q to dismiss"
     "placement": (placement-centered 0.4 0.4)})
```

## 7. See also

When a builtin belongs to a family or has a natural counterpart, point to it so
the reader can navigate. Comma-separated call forms, one line:

```
See also: (popup-hide), (popup-close), (popup-visible? NAME).
```

---

## Templates

**Trivial command** (no args, returns `()`):

```
(undo)

Undoes the last tracked edit, honoring the pending count prefix.
```

**Value query:**

```
(cursor-line)

Returns int: the focused buffer's cursor row, absolute and 0-indexed
(counts from the top of the buffer, not the viewport).
See also: (cursor-col), (cursor-screen-row).
```

**Rich builtin** (full template):

```
(popup-show NAME WIDGET [OPTS])

Opens the overlay panel named NAME, or updates it in place if a popup
with that name is already on the stack. Either way, raises it to the
top.

NAME   — ident | str: reuse a name to update without stacking; pick
         distinct names for popups visible at once.
WIDGET — widget: the tree drawn inside the popup's rect, usually
         (w-block PROPS (w-popup-self)) for a buf-backed popup.
OPTS   — map: optional. Recognized keys:
           "text":      str  — seed text (overwrites existing)
           "modes":     array of layer — keymap layers, specific last
           "placement": placement — see (placement-centered ...)
           "show-cursor": truthy to draw the cursor over the buf

Returns bufno: the backing buffer's opaque id.

Example:
  (popup-show 'help
    (w-block {"border": "rounded"} (w-popup-self))
    {"text": "press q to dismiss" "modes": ['popup]})

See also: (popup-hide), (popup-close), (popup-visible? NAME).
```

## Checklist

- [ ] Registered with `be_doc` / `bi_doc`, not `be` / `bi`.
- [ ] Signature names its parameters in UPPERCASE; `[ ]` for optional, `...` for
      variadic; no `/arity`.
- [ ] Blank line, then a sentence-case summary sentence first.
- [ ] Argument types drawn from the vocabulary above; non-obvious ones explained.
- [ ] Return stated when a value comes back; predicates say "1 if … else 0".
- [ ] Notable error modes noted; generic type errors left implicit.
- [ ] An example for anything with a map arg, callback, or non-trivial shape.
- [ ] Prose wrapped at ~72 columns; metavariables consistent across all sections.
