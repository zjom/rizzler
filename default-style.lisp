;; ============================================================================
;;  Velvety Twilight — bundled default theme
;; ============================================================================
;;
;;  Loaded after `default.lisp` (keybindings) and before the user's optional
;;  `init.lisp`, so any line below is overridable from user config.
;;
;;  The frame layout is described as a single widget tree returned by the fn
;;  passed to `(set-frame ...)`. The tree primitives are:
;;
;;    (vstack [...]) / (hstack [...])      — stack children with constraints
;;    (cells N child) / (min-cells N child) — child constraint inside a stack
;;    (fill N child) / (frac N M child)
;;    (line [span ...])                — single screen row
;;    (text "..." 'face)               — shorthand for one span
;;    (block child {...})              — bordered/titled box
;;    (editor-tree {...})              — the editor window tree leaf
;;    (minibuffer)                     — the minibuffer leaf
;;
;;  Faces, text properties, and overlays are unchanged from before.
;; ============================================================================


;; ---------------------------------------------------------------------------
;; 1. Palette + faces
;; ---------------------------------------------------------------------------

(let pal-bg-deep   (rgb   19   19   20))
(let pal-bg-line   (rgb   37   37   47))
(let pal-bg-sel    (rgb   52   55   56))
(let pal-bg-panel  (rgb   19   19   20))
(let pal-fg-base   (rgb  205  205  205))
(let pal-fg-dim    (rgb   96   96  119))
(let pal-accent    (rgb  180  150  220))
(let pal-warn      (rgb  235  192  133))
(let pal-error     (rgb  202  107  127))
(let pal-mode-i    (rgb  230  160   80))
(let pal-mode-v    (rgb  210  110  200))
(let pal-mode-c    (rgb  110  210  210))

;; The renderer looks up `default`, `selection`, `cursor-line` directly —
;; redefining these here drives the always-on built-in decorator passes.
(face-define 'default              {'fg: pal-fg-base "bg": pal-bg-deep})
(face-define "selection"           {"bg": pal-bg-sel})
(face-define "cursor-line"         {"bg": pal-bg-line "fg": pal-fg-base})

(face-define "twilight.muted"      {"fg": pal-fg-dim "italic": 1})
(face-define "twilight.accent"     {"fg": pal-accent "bold": 1})
(face-define "twilight.error"      {"fg": pal-error  "bold": 1 "underline": 1})
(face-define "twilight.warn"       {"fg": pal-warn   "bold": 1})

(face-define "twilight.mode.normal"       {"fg": 'green       "bold": 1})
(face-define "twilight.mode.insert"       {"fg": pal-mode-i   "bold": 1})
(face-define "twilight.mode.visual"       {"fg": pal-mode-v   "bold": 1})
(face-define "twilight.mode.visual-line"  {"fg": pal-mode-v   "bold": 1 "italic": 1})
(face-define "twilight.mode.visual-block" {"fg": pal-mode-v   "bold": 1 "underline": 1})
(face-define "twilight.mode.command"      {"fg": pal-mode-c   "bold": 1})

(face-define "twilight.gutter"          {"fg": pal-fg-dim})
(face-define "twilight.gutter-current"  {"fg": pal-accent       "bold": 1})

(face-define "twilight.signature"  {"fg": 215 "italic": 1})
(face-define "twilight.reverse"    {"reverse": 1 "bold": 1})

(face-define "twilight.everything"
  {"fg": (rgb 255 240 200)
   "bg": (rgb  60  20  60)
   "bold":      1
   "italic":    1
   "underline": 1
   "reverse":   0})


;; ---------------------------------------------------------------------------
;; 2. Helpers shared by the layout fn
;; ---------------------------------------------------------------------------

(fn _mode-face (m)
  (if (= m "normal")       "twilight.mode.normal"
  (if (= m "insert")       "twilight.mode.insert"
  (if (= m "visual")       "twilight.mode.visual"
  (if (= m "visual-line")  "twilight.mode.visual-line"
  (if (= m "visual-block") "twilight.mode.visual-block"
  (if (= m "command")      "twilight.mode.command"
                           "twilight.muted")))))))

(fn _mode-glyph (m)
  (if (= m "normal")       "N"
  (if (= m "insert")       "I"
  (if (= m "visual")       "V"
  (if (= m "visual-line")  "L"
  (if (= m "visual-block") "B"
  (if (= m "command")      "C"
                           "?")))))))

(fn _pad-right (s w)
  (do
    (let pad (- w (len s)))
    (if (> pad 0)
        (str-join [s (str-join (fmap (fn _spc (_) " ") (range 0 pad)) "")] "")
        s)))


;; ---------------------------------------------------------------------------
;; 3. Gutter
;; ---------------------------------------------------------------------------
;;
;; The gutter fn is called per visible row with `lnum` set to the file row,
;; or `()` for rows past EOF. Returns the styled content for one row of the
;; gutter. The total column width is reserved by `gutter-width` on the
;; editor-tree widget.

(fn _gutter (n)
  (if (= n ())
      (text "     " "twilight.gutter")
      (if (= n (cursor-line))
          (text (_pad-right (str-join ["▎ " (to-str n)] "") 5)
                "twilight.gutter-current")
          (text (_pad-right (str-join ["│ " (to-str n)] "") 5)
                "twilight.gutter"))))


;; ---------------------------------------------------------------------------
;; 4. Status-line segments
;; ---------------------------------------------------------------------------
;;
;; Each segment is a fn returning a span. They get spliced into the status
;; line by `_status-line` below.

(fn _seg-mode ()
  (do
    (let m (focused-mode))
    (text (str-join [" " (_mode-glyph m) " "] "")
          (_mode-face m))))

(fn _seg-buf-path ()
  (do
    (let path (buf-path))
    (if (= path ())
        (text "  twilight  " "twilight.signature")
        (text path "twilight.signature"))))

(fn _seg-selection-hint ()
  (do
    (let sel (selected-text))
    (if (= sel ())
        (text "" "twilight.muted")
        (text (str-join [" " (to-str (len sel)) " chars selected "] "")
              "twilight.accent"))))

(fn _seg-cursor ()
  (text (str-join [(to-str (cursor-line)) ":" (to-str (cursor-col))] "")
        "twilight.accent"))

(fn _seg-bufno ()
  (do
    (let m (focused-mode))
    (if (= m "command")
        (text (to-str (buf-no)) "twilight.reverse")
        (text (to-str (buf-no)) "twilight.warn"))))


;; ---------------------------------------------------------------------------
;; 5. Bottom hint bar
;; ---------------------------------------------------------------------------

(fn _hint-bar ()
  (do
    (let m (focused-mode))
    (if (= m "normal")
        (line [(text "  press : for commands · i to insert · v to select  "
                     "twilight.muted")])
    (if (= m "insert")
        (line [(text "  press <esc> to leave insert mode  " "twilight.muted")])
    (if (= m "command")
        (line [(text "  command mode — type a form and press <enter>  "
                     "twilight.muted")])
        (line [(text "  visual: y to yank · d to delete · <esc> to cancel  "
                     "twilight.muted")]))))))


;; ---------------------------------------------------------------------------
;; 6. Frame layout
;; ---------------------------------------------------------------------------
;;
;; Vertical stack, top-to-bottom:
;;
;;    editor windows  (min-cells 1)   ← grows to fill
;;    status line     (cells 1)
;;    bottom hint     (cells 1)
;;    minibuffer      (cells 1)
;;
;; The status line is an hstack with a `fill` spacer between the left and
;; right groups so right-aligned segments hug the right margin.

(fn _status-line ()
  (hstack
    [(min-cells 1
      (line [(_seg-mode)
             (_seg-buf-path)
             (_seg-selection-hint)]))
     (fill 1
      (right-align
        (line [(_seg-cursor)
               (text " • " "twilight.muted")
               (text (last-key) "twilight.muted")
               (text "  " "twilight.muted")
               (_seg-bufno)])))]))

(fn _frame ()
  (vstack
    [(min-cells 1
      (editor-tree
        {"gutter":       _gutter
         "gutter-width": 5}))
     (cells 1 (_status-line))
     (cells 1 (_hint-bar))
     (cells 1 (minibuffer))]))

(set-frame _frame)


;; ---------------------------------------------------------------------------
;; 7. Overlays + virtual text (unchanged: buffer-attached, not widgets)
;; ---------------------------------------------------------------------------

(face-define "demo.lint.error"   {"fg": pal-error  "underline": 1})
(face-define "demo.lint.warn"    {"fg": pal-warn   "italic": 1})
(face-define "demo.ghost"        {"fg": pal-fg-dim "italic": 1})
(face-define "demo.fold"         {"fg": pal-accent "bg": pal-bg-line "bold": 1})
(face-define "demo.highlight"    {"bg": (rgb 50 30 70)})
(face-define "demo.match"        {"fg": pal-bg-deep "bg": pal-warn "bold": 1})

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

(fn _ov (sr sc er ec face props)
  (do
    (let id (overlay-create sr sc er ec face))
    (if (!= (get props "display") ())
        (overlay-put id "display" (get props "display"))
        ())
    (if (!= (get props "priority") ())
        (overlay-put id "priority" (get props "priority"))
        ())
    (if (!= (get props "pad-to-width") ())
        (overlay-put id "pad-to-width" (get props "pad-to-width"))
        ())
    id))

(fn overlays-demo ()
  (do
    (buf-text-set (buf-no) _demo-text)
    (clear-text-properties)
    (put-text-property 1 19 1 26 "demo.lint.error")
    (put-text-property 6 3 6 7 "demo.lint.warn")
    (_ov 6 0 6 0 "demo.highlight"   {"pad-to-width": 1})
    (_ov 0 35 0 46 "demo.ghost"      {"display": " &str "})
    (_ov 2 4 2 28 "demo.fold"        {"display": "  ⋯ println …  "})
    (_ov 8 16 8 47 "twilight.muted"  {"priority": 1})
    (_ov 8 26 8 36 "twilight.accent" {"priority": 10})
    (_ov 0 4  0 9  "demo.match" {})
    (_ov 3 11 3 16 "demo.match" {})
    (_ov 10 8 10 13 "demo.match" {})
    (_ov 1 27 1 38 "twilight.reverse" {"display": {"space": 7}})
    (notify "overlays-demo: applied. try :(overlays-clear)")))

(keymap-set 'normal "od" '(overlays-demo))

(fn overlays-clear ()
  (do
    (clear-text-properties)
    (buf-text-set (buf-no) "")
    (notify "overlays-clear: text-properties cleared, buffer reset")))


;; ---------------------------------------------------------------------------
;; 8. Popups
;; ---------------------------------------------------------------------------
;;
;; Popup faces, reused by `notify` / `messages` / `popup-help` / `popup-files`
;; in `default.lisp`.

(face-define "popup.default"     {"fg": pal-fg-base "bg": pal-bg-panel})
(face-define "popup.border"      {"fg": pal-accent  "bg": pal-bg-panel})
(face-define "popup.title"       {"fg": pal-warn    "bg": pal-bg-panel "bold": 1})
(face-define "popup.dir"         {"fg": pal-accent  "bold": 1})
(face-define "popup.file"        {"fg": pal-fg-base})

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
