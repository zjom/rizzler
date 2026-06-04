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
;; picked up automatically. You can use either a str or an ident to refer to them.
(face-define 'default             {'fg: pal-fg-base "bg": pal-bg-deep})
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

;; --- right: buffer number ---------------------------------------------
(fn _bufno ()
  (do
    (let m (focused-mode))
    ;; In command mode, highlight the buffer index using `reverse` for emphasis.
    (if (= m "command")
        (span (to-str (buf-no)) "twilight.reverse")
        (span (to-str (buf-no)) "twilight.warn"))
      ))

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
;; 7. Composition
;; ---------------------------------------------------------------------------
;;
;; Since this is lisp, you can compose your components and styles however you wish.

(fn _banner ()
    [[(span "  rizz  " "twilight.error")  (_bufno) (first (_hint-bar)) ]])

(region-add 'banner 'top _banner)


;; ---------------------------------------------------------------------------
;; 8. Sanity / debug
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


;; ---------------------------------------------------------------------------
;; 9. Overlays + virtual text
;; ---------------------------------------------------------------------------
;;
;; Decorators (section 4) paint per-frame, recomputed each render. Overlays
;; and text-properties are the *other* path: they live on the buffer itself,
;; attach to absolute (row, col) ranges, and are emitted as styled ranges by
;; `props.rs::build_prop_ranges`. Two flavors:
;;
;;   * `put-text-property` — anonymous, batch-cleared with
;;     `clear-text-properties`. Good for things you rebuild on every change
;;     (a syntax pass, a lint result set).
;;
;;   * `overlay-create` — returns an integer handle. `overlay-put` mutates a
;;     single overlay (face / priority / display / pad-to-width).
;;     `overlay-delete` removes it. Use this when you want to flip one
;;     individual annotation without re-emitting the rest.
;;
;; Overlays sort by ascending priority before emission, so higher priority
;; lands on top. The `display` key swaps the underlying chars for a
;; substitute — that's the "virtual text" mechanism (fold ellipses, inline
;; hints, ghost completions). It's single-row only.
;;
;; The functions below take effect on the *currently focused* buffer when
;; invoked. From command mode (`:`) run `(overlays-demo)` to populate a
;; buffer with sample content + the full overlay set, or
;; `(overlays-clear)` to wipe it.

;; --- demo-specific faces ------------------------------------------------
(face-define "demo.lint.error"
  {"fg": pal-error  "underline": 1})
(face-define "demo.lint.warn"
  {"fg": pal-warn   "italic": 1})
(face-define "demo.ghost"                ;; virtual-text / ghost completion
  {"fg": pal-fg-dim "italic": 1})
(face-define "demo.fold"                 ;; collapsed region ellipsis
  {"fg": pal-accent "bg": pal-bg-line "bold": 1})
(face-define "demo.highlight"            ;; full-width band
  {"bg": (rgb 50 30 70)})
(face-define "demo.match"                ;; search-style hit
  {"fg": pal-bg-deep "bg": pal-warn "bold": 1})

;; --- sample buffer content the demo annotates ---------------------------
;; Twelve lines so every overlay below has somewhere to land. Lisp doesn't
;; have multi-line string literals here, so we assemble with `str-join`.
(let _demo-text
  (str-join
    ["fn greet(name: String) -> String {"
     "    let greeting = format!(\"hello, {}\", name);"
     "    println!(\"{}\", greeting);"
     "    return greeting;"
     "}"
     ""
     "// TODO: support multiple languages"
     "fn main() {"
     "    let names = vec![\"world\", \"twilight\", \"rizz\"];"
     "    for n in names {"
     "        greet(n.to_string());"
     "    }"
     "}"]
    "\n"))

;; --- helper: create an overlay and immediately apply a key/value bag ----
;; Wraps the three calls (create, put-face, put-display, put-priority...)
;; in one place so the demo below reads as data. Returns the overlay id.
(fn _ov (sr sc er ec face props)
  (do
    (let id (overlay-create sr sc er ec face))
    ;; `props` is a map of "key": value entries we forward to overlay-put.
    ;; Keys understood: "display", "priority", "pad-to-width".
    (if (! (= (get props "display") ()))
        (overlay-put id "display" (get props "display"))
        ())
    (if (! (= (get props "priority") ()))
        (overlay-put id "priority" (get props "priority"))
        ())
    (if (! (= (get props "pad-to-width") ()))
        (overlay-put id "pad-to-width" (get props "pad-to-width"))
        ())
    id))

;; --- the demo entrypoint ------------------------------------------------
;; Stamps the sample buffer text, then layers:
;;
;;   1. text properties  — anonymous lint marks on lines 0 and 6
;;   2. an overlay       — full-width highlight on the TODO line
;;   3. virtual text     — a ghost type-hint and a fold ellipsis
;;   4. priority         — two overlapping overlays where the higher wins
;;   5. an inline match  — search-style hit, padded to one cell width
;;
;; Re-runnable: it clears prior demo state first.
(fn overlays-demo ()
  (do
    ;; Replace the focused buffer's contents with the sample. `buf-no`
    ;; gives the focused buffer index, which is what `buf-text-set` wants.
    (buf-text-set (buf-no) _demo-text)

    ;; Wipe any prior demo state so re-running is idempotent. Anonymous
    ;; properties get cleared in bulk; individual overlays would need their
    ;; ids tracked — we leave them alone here for simplicity, and recommend
    ;; calling `(overlays-clear)` between iterations during exploration.
    (clear-text-properties)

    ;; 1. Lint-style text properties — these are the "fire and forget"
    ;;    form: no handle returned, cleared in bulk later.
    ;;    Underline `format!` on line 1.
    (put-text-property 1 19 1 26 "demo.lint.error")
    ;;    Italicize the TODO marker on line 6.
    (put-text-property 6 3 6 7 "demo.lint.warn")

    ;; 2. Full-width band on the TODO line. `pad-to-width` extends the
    ;;    highlight past the end of the actual characters so the band
    ;;    reaches the right margin (same trick as `cursor-line`).
    (_ov 6 0 6 0 "demo.highlight"
         {"pad-to-width": 1})

    ;; 3a. Virtual text — a ghost type hint after the `name` parameter on
    ;;     line 0. We attach to a single cell and *replace* it with new
    ;;     text via `display`. The substituted content can be longer than
    ;;     the original range; the renderer pushes following chars right.
    (_ov 0 35 0 46 "demo.ghost"
         {"display": " &str "})

    ;; 3b. Fold ellipsis — collapse the println line by replacing its
    ;;     entire content with a single token. (Real fold UX would also
    ;;     hide the following rows; this just demonstrates the inline
    ;;     substitution mechanism.)
    (_ov 2 4 2 28 "demo.fold"
         {"display": "  ⋯ println …  "})

    ;; 4. Priority layering — two overlapping overlays on line 8.
    ;;    The first paints the whole vec! range muted; the second
    ;;    repaints just "twilight" with a brighter accent because its
    ;;    priority is higher.
    (_ov 8 16 8 47 "twilight.muted"
         {"priority": 1})
    (_ov 8 26 8 36 "twilight.accent"
          {"priority": 10})

    ;; 5. Search-style match — every `greet` identifier in the file.
    ;;    Listing the positions inline is clearest for a demo; a real
    ;;    search would walk the buffer.
    (_ov 0 4  0 9  "demo.match" {})
    (_ov 3 11 3 16 "demo.match" {})
    (_ov 10 8 10 13 "demo.match" {})

    ;; 6. A `{"space": N}` display — replaces a range with N blank cells,
    ;;    styled. Useful for visually "redacting" content (e.g. secrets in
    ;;    a log buffer) without changing buffer length.
    (_ov 1 27 1 38 "twilight.reverse"
         {"display": {"space": 7}})

    (message "overlays-demo: applied. try :(overlays-clear)")))

(keymap-set 'normal "od" '(overlays-demo))

;; --- teardown -----------------------------------------------------------
;; `clear-text-properties` only wipes the anonymous set. Overlays are
;; per-handle and would each need `(overlay-delete id)`. For an exploration
;; session, the simplest reset is to reload the buffer (close + reopen);
;; this entrypoint just clears the anonymous half and refreshes the text.
(fn overlays-clear ()
  (do
    (clear-text-properties)
    (buf-text-set (buf-no) "")
    (message "overlays-clear: text-properties cleared, buffer reset")))


;; ---------------------------------------------------------------------------
;; 10. Popups
;; ---------------------------------------------------------------------------
;;
;; The same primitive that drives `(notify ...)` exposes a generalized
;; overlay: a popup is conceptually a buffer drawn on top of the editor area,
;; with chrome (border / title), placement, and a keymap mode that captures
;; input while the popup is on top of the stack. That means a popup terminal,
;; file explorer, completion list, or hover-doc is each just `(popup-open …)`
;; with a different `mode` and different content fed into the popup's buffer.
;;
;; Faces, placement, and the keymap registry are reused as-is — popup chrome
;; references face names like any other styled span, and `keymap-set` on a
;; popup mode uses the same surface as `'normal` or `'insert`.

;; --- chrome faces, reusing the palette above ---------------------------
(face-define "popup.default"     {"fg": pal-fg-base "bg": pal-bg-panel})
(face-define "popup.border"      {"fg": pal-accent  "bg": pal-bg-panel})
(face-define "popup.title"       {"fg": pal-warn    "bg": pal-bg-panel "bold": 1})
(face-define "popup.dir"         {"fg": pal-accent  "bold": 1})
(face-define "popup.file"        {"fg": pal-fg-base})

;; --- (popup-help)  bottom-anchored cheat-sheet popup -------------------
;; Demonstrates the `side` placement, a rounded border, and a custom title
;; — but uses the default `'popup` keymap mode so `j/k/q/<esc>` work without
;; any extra binding.
(fn popup-help ()
  (popup-open
    {"text": (str-join
              ["popup cheat-sheet"
               "──────────────────────────"
               "j / k         scroll one line"
               "<c-d> / <c-u> half page"
               "g / G         top / bottom"
               "q / <esc>     dismiss"
               ""
               "tip: any popup defaulting to mode 'popup picks up these keys."]
              "\n")
     "placement":   {"kind": "side" "side": "bottom" "size": 12}
     "border":      "rounded"
     "title":       " help "
     "face":        "popup.default"
     "border-face": "popup.border"
     "title-face":  "popup.title"}))

(keymap-set 'normal "?" '(popup-help))


;; --- (popup-files)  centered file-explorer popup -----------------------
;; Reuses the buffer machinery (the popup buffer holds the directory
;; listing as plain text) and a *custom* keymap mode (`'popup.files`) so we
;; can attach explorer-specific bindings without conflicting with the
;; default popup mode. Pressing `<enter>` on a row opens that path; `q`
;; dismisses.

;; Snapshot of the directory we listed, indexed by line number. Updated on
;; each `(popup-files)` invocation so `<enter>` knows which path the cursor
;; is on without parsing the buffer text back.
(let _popup-files-entries (ref []))

(fn _popup-files-render (dir)
  (do
    (let entries (fs-readdir dir))
    (set! _popup-files-entries entries)
    (str-join
      (fmap (fn _row (p) (to-str p)) entries)
      "\n")))

(fn popup-files ()
  (do
    (let dir (workdir))
    (popup-open
      {"text":        (_popup-files-render dir)
       "mode":        'popup.files
       "buffer-mode": 'normal
       "placement":   {"kind": "center" "w": 0.5 "h": 0.5}
       "border":      "rounded"
       "title":       (str-join [" files: " (to-str dir) " "] "")
       "face":        "popup.default"
       "border-face": "popup.border"
       "title-face":  "popup.title"
       "show-cursor": 1})))

;; Bind the explorer to `<space>f`. Movement keys are shared with the
;; default popup mode, but `q`/`<esc>` still need to be wired up since
;; `'popup.files` has its own keymap.
(keymap-set 'normal "<space>f" '(popup-files))

(keymap-set 'popup.files "j"        '(move-cursor 'down))
(keymap-set 'popup.files "k"        '(move-cursor 'up))
(keymap-set 'popup.files "<down>"   '(move-cursor 'down))
(keymap-set 'popup.files "<up>"     '(move-cursor 'up))
(keymap-set 'popup.files "<c-d>"    '(move-cursor 'half-page-down))
(keymap-set 'popup.files "<c-u>"    '(move-cursor 'half-page-up))
(keymap-set 'popup.files "gg"       '(move-cursor 'file-start))
(keymap-set 'popup.files "G"        '(move-cursor 'file-end))
(keymap-set 'popup.files "q"        '(popup-close))
(keymap-set 'popup.files "<esc>"    '(popup-close))
;; `<enter>` reads the current line out of the popup buffer (via
;; `selected-text`/`buf-text`) and asks the editor to edit it. The popup
;; closes first so the new buffer takes focus cleanly.
(keymap-set 'popup.files "<enter>"
  '(do
     (let line (cursor-line))
     (let entries (deref _popup-files-entries))
     (let target (get entries line))
     (popup-close)
     (if (= target ())
         ()
     (if (fs-isdir target)
          (popup-open
            {"text":        (_popup-files-render target)
             "mode":        'popup.files
             "buffer-mode": 'normal
             "placement":   {"kind": "center" "w": 0.5 "h": 0.5}
             "border":      "rounded"
             "title":       (str-join [" files: " target " "] "")
             "face":        "popup.default"
             "border-face": "popup.border"
             "title-face":  "popup.title"
             "show-cursor": 1})
     (edit target)
     ))
))


(keymap-set 'popup.files "h"
   '(do (let entries (deref _popup-files-entries))
        (let target (fs-parent (fs-parent (first entries))))
        (popup-close)
        (if (= target ())
          ()
        (if (fs-isdir target)
          (popup-open
            {"text":        (_popup-files-render target)
             "mode":        'popup.files
             "buffer-mode": 'normal
             "placement":   {"kind": "center" "w": 0.5 "h": 0.5}
             "border":      "rounded"
             "title":       (str-join [" files: " target " "] "")
             "face":        "popup.default"
             "border-face": "popup.border"
             "title-face":  "popup.title"
             "show-cursor": 1})
          ()
        ))
     ))
