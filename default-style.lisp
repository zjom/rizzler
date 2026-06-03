;; ============================================================================
;;  Velvety Twilight — bundled default theme
;; ============================================================================
;;
;;  Loaded after `default.lisp` (keybindings) and before the user's optional
;;  `init.lisp`, so any line below is overridable from user config.
;;
;;  This file is intentionally maximal: it exercises every option the
;;  styling/render framework exposes, so it doubles as a worked reference
;;  for `~/.config/editor/init.lisp`.
;;
;;  ----------------------------------------------------------------------------
;;  Quick map of what's shown:
;;
;;    Faces      — every attribute (fg / bg / bold / italic / underline /
;;                 reverse) across every color form (named ident, xterm
;;                 indexed int, and `(rgb r g b)` true-color).
;;    Helpers    — `fn` defines, `let`, `if`, `do`, `fmap`, `range`, arithmetic,
;;                 `str-join`, `to-str`, `=`, `!`.
;;    Regions    — `region-add` for every anchor (status-left, status-right,
;;                 gutter, decorator, top, bottom) in every payload form
;;                 (Builtin / Static / Callable).
;; ============================================================================


;; ---------------------------------------------------------------------------
;; 1. Palette + faces
;; ---------------------------------------------------------------------------

;; --- the palette: bind colors once so the rest of the file reads cleanly ---
(let pal-bg-deep   (rgb  18  18  30))   ;; deepest backdrop
(let pal-bg-line   (rgb  32  32  52))   ;; current-line band
(let pal-bg-sel    (rgb  20  60 110))   ;; visual selection
(let pal-bg-panel  (rgb  26  26  42))   ;; bottom strip
(let pal-fg-base   (rgb 220 220 240))   ;; default text
(let pal-fg-dim    (rgb 110 110 140))   ;; comments / past-EOF / hints
(let pal-accent    (rgb 180 150 220))   ;; lavender accent
(let pal-warn      (rgb 230 180  60))   ;; warning amber
(let pal-error     (rgb 230  90  90))   ;; error red
(let pal-mode-i    (rgb 230 160  80))   ;; insert glow
(let pal-mode-v    (rgb 210 110 200))   ;; visual magenta
(let pal-mode-c    (rgb 110 210 210))   ;; command cyan

;; --- base text + frame ---------------------------------------------------
;; `default`, `selection`, and `cursor-line` are the canonical names the Rust
;; renderer looks up: `default` fills the whole frame as the baseline fg/bg,
;; `selection` colors the visual selection band, `cursor-line` colors the
;; current-line band. Any face the user redefines under these names is
;; picked up automatically.
(face-define "default"             {"fg": pal-fg-base "bg": pal-bg-deep})
(face-define "selection"              {"bg": pal-bg-sel})
(face-define "cursor-line"         {"bg": pal-bg-line "fg": pal-fg-base})

(face-define "twilight.muted"      {"fg": pal-fg-dim "italic": 1})
(face-define "twilight.accent"     {"fg": pal-accent "bold": 1})

;; --- diagnostics: showcase bold + underline together --------------------
(face-define "twilight.error"      {"fg": pal-error  "bold": 1 "underline": 1})
(face-define "twilight.warn"       {"fg": pal-warn   "bold": 1})

;; --- per-mode badges: every visual mode gets its own face --------------
(face-define "twilight.mode.normal"       {"fg": 'green       "bold": 1})
(face-define "twilight.mode.insert"       {"fg": pal-mode-i   "bold": 1})
(face-define "twilight.mode.visual"       {"fg": pal-mode-v   "bold": 1})
(face-define "twilight.mode.visual-line"  {"fg": pal-mode-v   "bold": 1 "italic": 1})
(face-define "twilight.mode.visual-block" {"fg": pal-mode-v   "bold": 1 "underline": 1})
(face-define "twilight.mode.command"      {"fg": pal-mode-c   "bold": 1})

;; --- gutter + cursor ----------------------------------------------------
(face-define "twilight.gutter"          {"fg": pal-fg-dim})
(face-define "twilight.gutter-current"  {"fg": pal-accent       "bold": 1})
(face-define "twilight.cursor-marker"   {"fg": pal-bg-deep      "bg": pal-accent})

;; --- xterm-indexed example: a soft amber, color 215 ---------------------
(face-define "twilight.signature"  {"fg": 215 "italic": 1})

;; --- reverse-video example: emergency emphasis --------------------------
(face-define "twilight.reverse"    {"reverse": 1 "bold": 1})

;; --- a face that uses every attribute at once (just to prove it works) -
(face-define "twilight.everything"
  {"fg": (rgb 255 240 200)
   "bg": (rgb  60  20  60)
   "bold":      1
   "italic":    1
   "underline": 1
   "reverse":   0})           ;; explicit 0 — falsy, so reverse stays off


;; ---------------------------------------------------------------------------
;; 2. Helpers
;; ---------------------------------------------------------------------------

;; Map a mode name to its face. Showcases `if` chained for a small switch.
(fn _mode-face (m)
  (if (= m "normal")       "twilight.mode.normal"
  (if (= m "insert")       "twilight.mode.insert"
  (if (= m "visual")       "twilight.mode.visual"
  (if (= m "visual-line")  "twilight.mode.visual-line"
  (if (= m "visual-block") "twilight.mode.visual-block"
  (if (= m "command")      "twilight.mode.command"
                           "twilight.muted")))))))

;; Mode glyph in a single letter. `do` sequences nothing here but the
;; if-chain still threads through one expression cleanly.
(fn _mode-glyph (m)
  (if (= m "normal")       "N"
  (if (= m "insert")       "I"
  (if (= m "visual")       "V"
  (if (= m "visual-line")  "L"
  (if (= m "visual-block") "B"
  (if (= m "command")      "C"
                           "?")))))))

;; Right-pad a string to `w` columns using `str-join` and `range` /
;; `fmap` to build a space-string of the right length. `do` lets us bind a
;; local before the return expression.
(fn _pad-right (s w)
  (do
    (let pad (- w (len s)))
    (if (> pad 0)
        (str-join [s (str-join (fmap (fn _spc (_) " ") (range 0 pad)) "")] "")
        s)))


;; ---------------------------------------------------------------------------
;; 3. Gutters
;; ---------------------------------------------------------------------------
;;
;; A 5-column lisp gutter that draws a marker (▎) on the cursor line and a
;; faint pipe (│) elsewhere. Wraps both in faces, so the colors come from the
;; palette above. Past-EOF rows (lnum = `()`) render as plain whitespace.
;;
;; If you want the original right-aligned bare numbers back, replace this
;; with:  (region-add 'line-numbers {"gutter": 0} 'line-numbers)

(fn _gutter (n)
  (if (= n ())
      (span "     " "twilight.gutter")
      (if (= n (cursor-line))
          (span (_pad-right (str-join ["▎ " (to-str n)] "") 5)
                "twilight.gutter-current")
          (span (_pad-right (str-join ["│ " (to-str n)] "") 5)
                "twilight.gutter"))))

(region-add 'line-numbers {"gutter": 5} _gutter)


;; ---------------------------------------------------------------------------
;; 4. Line decorators
;; ---------------------------------------------------------------------------
;;
;; Three Rust-built-in decorators paint the base foreground, the selection
;; band, and the current-line band — they're declared by name (an ident
;; that resolves to a `BuiltinId`).
;;
;; A fourth lisp-defined decorator paints a single accent cell at the exact
;; cursor column on the current line. Showcases:
;;   * a callable handler returning a list of `{row col len style ...}` maps
;;   * an inline style map mixed with face-name references
;;   * order matters — registered last, so it layers over current-line-bg.

(region-add 'base-fg              'decorator 'base-fg)
(region-add 'selection-highlight  'decorator 'selection-highlight)

(fn _current-line ()
  [{"row":          (cursor-line)
    "col":          0
    "len":          0
    "style":        "cursor-line"
    "pad-to-width": 1}])
(region-add 'current-line-highlight 'decorator _current-line)

(fn _cursor-marker ()
  ;; Single-cell array of one StyledRange map. Could be empty (`[]`) and
  ;; that's a valid decorator output too.
  [{"row":          (cursor-line)
    "col":          (cursor-col)
    "len":          1
    "style":        "twilight.cursor-marker"
    "pad-to-width": 0}])

(region-add 'cursor-marker 'decorator _cursor-marker)


;; ---------------------------------------------------------------------------
;; 5. Status line
;; ---------------------------------------------------------------------------
;;
;; Clean slate, then rebuild. `region-remove` is idempotent so user
;; configs can layer further.

(region-remove 'mode-glyph)
(region-remove 'last-key)
(region-remove 'spacer)
(region-remove 'buffer-no)

;; --- left: mode badge ---------------------------------------------------
(fn _mode-segment ()
  (do
    (let m (focused-mode))
    (span (str-join [" " (_mode-glyph m) " "] "")
          (_mode-face m))))

(region-add 'mode 'status-left _mode-segment)

;; --- left: current buffer file path ----------------------
(fn _buf-path ()
  (do
    (let path (buf-path))
    (let content
      (if (= path ())
        "  twilight  "
        path))
    (span content "twilight.signature")))
(region-add 'buffer-path 'status-left _buf-path)

;; --- left: contextual hint depending on selection ----------------------
;; `selected-text` returns `()` when nothing's selected, otherwise the text.
(fn _selection-hint ()
  (do
    (let sel (selected-text))
    (if (= sel ())
        ""                                       ;; no selection: empty span
        (span (str-join [" " (to-str (len sel)) " chars selected "] "")
              "twilight.accent"))))

(region-add 'sel-hint 'status-left _selection-hint)

;; --- right: cursor position ------------------------------------------
(fn _cursor-pos ()
  (span (str-join [(to-str (cursor-line)) ":" (to-str (cursor-col))] "")
        "twilight.accent"))

(region-add 'cursor 'status-right _cursor-pos)

;; --- right: a static dividing pip ------------------------------------
(region-add 'pip 'status-right
  (span " • " "twilight.muted"))

;; --- right: builtin last-key reference, kept verbatim ----------------
(region-add 'last-key 'status-right 'last-key)

;; --- right: spacer (Static plain string — simplest possible payload) -
(region-add 'spacer 'status-right "  ")

;; --- right: builtin buffer number, styled by composing with `span`  --
;; `buffer-no` is a Rust builtin, but we can also wrap it in a closure to
;; restyle it.
(fn _bufno ()
  (do
    (let m (focused-mode))
    ;; In command mode, highlight the buffer index using `reverse` for emphasis.
    (if (= m "command")
        (span (to-str (cursor-line)) "twilight.reverse")
        (span (to-str (cursor-line)) "twilight.warn"))))

(region-add 'bufno 'status-right _bufno)


;; ---------------------------------------------------------------------------
;; 6. Bottom strip
;; ---------------------------------------------------------------------------
;;
;; Bottom-strip components slot between the status line and the minibuffer.
;; A handler that returns `[[span...] [span...] ...]` lays out N rows; here
;; we produce a single one-row tip line.
;;
;; To remove: `(region-remove 'hint-bar)` in your `init.lisp`.

(fn _hint-bar ()
  (do
    (let m (focused-mode))
    (let lhs
      (if (= m "normal")
          (span "  press : for commands · i to insert · v to select  "
                "twilight.muted")
      (if (= m "insert")
          (span "  press <esc> to leave insert mode  " "twilight.muted")
      (if (= m "command")
          (span "  command mode — type a form and press <enter>  "
                "twilight.muted")
          (span "  visual: y to yank · d to delete · <esc> to cancel  "
                "twilight.muted")))))
    ;; Returns one row, where the row is an array of spans laid left-to-right.
    [[lhs]]))

(region-add 'hint-bar 'bottom _hint-bar)


;; ---------------------------------------------------------------------------
;; 7. Sanity / debug
;; ---------------------------------------------------------------------------
;;
;; A no-op showcase of arithmetic + `fmap` + `range` building data the engine
;; will happily store but never display (decorators with `len: 0` and no pad
;; produce zero visible effect).
;;
;; Demonstrates that the framework happily accepts richer programmatic input.

(fn _phantom-ranges ()
  (fmap (fn _to-range (i)
          {"row":          (+ (cursor-line) i)
           "col":          0
           "len":          0
           "style":        "twilight.everything"
           "pad-to-width": 0})
        (range 1 1)))                     ;; empty range — produces no entries

(region-add 'phantom 'decorator _phantom-ranges)
