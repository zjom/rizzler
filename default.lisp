;; Default keybindings, bundled into the editor binary and evaluated by
;; `State::with_config` before any user `init.lisp`. Mirrors the layout that
;; used to live in `src/keymap/default.rs`.

;; ----- normal mode -----------------------------------------------------
(let map keymap-set)
(map 'normal ":"      '(set-mode 'command))
(map 'normal "v"      '(set-mode 'visual))
(map 'normal "V"      '(set-mode 'visual-line))
(map 'normal "<c-v>"  '(set-mode 'visual-block))

(map 'normal "i"      '(set-mode 'insert))
(map 'normal "I"      '(do (move-cursor 'line-start)
                           (set-mode 'insert)))
(map 'normal "a"      '(do (set-mode 'insert)
                           (move-cursor 'right)))
(map 'normal "A"      '(do (set-mode 'insert)
                           (move-cursor 'line-end)
                           (move-cursor 'right)))
(map 'normal "o"      '(do (set-mode 'insert)
                           (move-cursor 'line-end)
                           (newline)))
(map 'normal "O"      '(do (move-cursor 'line-start)
                           (set-mode 'insert)
                           (newline)
                           (move-cursor 'up)))

(map 'normal "x"      '(delete-char-at (cursor-col) (cursor-line)))

;; vim `dd` deletes the current line; `d<motion>` deletes from the cursor to
;; the motion's destination. Each combo is its own keymap entry — the trie
;; resolves them as multi-key sequences.
(map 'normal "dd"     '(delete-line))
(map 'normal "dh"     '(delete-motion 'left))
(map 'normal "dl"     '(delete-motion 'right))
(map 'normal "dj"     '(delete-motion 'down))
(map 'normal "dk"     '(delete-motion 'up))
(map 'normal "d0"     '(delete-motion 'line-start))
(map 'normal "d^"     '(delete-motion 'line-first-non-blank))
(map 'normal "d$"     '(delete-motion 'line-end))
(map 'normal "dw"     '(delete-motion 'word-forward))
(map 'normal "dW"     '(delete-motion 'big-word-forward))
(map 'normal "db"     '(delete-motion 'word-start))
(map 'normal "dB"     '(delete-motion 'big-word-start))
(map 'normal "de"     '(delete-motion 'word-end))
(map 'normal "dE"     '(delete-motion 'big-word-end))
(map 'normal "dge"    '(delete-motion 'word-back-end))
(map 'normal "dgE"    '(delete-motion 'big-word-back-end))
(map 'normal "dgg"    '(delete-motion 'file-start))
(map 'normal "dG"     '(delete-motion 'file-end))
(map 'normal "d%"     '(delete-motion 'match-bracket))
(map 'normal "u"      '(undo))
(map 'normal "<c-r>"  '(redo))
(map 'normal "j"      '(move-cursor 'down))
(map 'normal "<down>" '(move-cursor 'down))
(map 'normal "k"      '(move-cursor 'up))
(map 'normal "<up>"   '(move-cursor 'up))
(map 'normal "h"      '(move-cursor 'left))
(map 'normal "<left>" '(move-cursor 'left))
(map 'normal "l"      '(move-cursor 'right))
(map 'normal "<right>" '(move-cursor 'right))

(map 'normal "0"      '(move-cursor 'line-start))
(map 'normal "^"      '(move-cursor 'line-first-non-blank))
(map 'normal "$"      '(move-cursor 'line-end))
(map 'normal "gg"     '(move-cursor 'file-start))
(map 'normal "G"      '(move-cursor 'file-end))
(map 'normal "b"      '(move-cursor 'word-start))
(map 'normal "B"      '(move-cursor 'big-word-start))
(map 'normal "w"      '(move-cursor 'word-forward))
(map 'normal "W"      '(move-cursor 'big-word-forward))
(map 'normal "e"      '(move-cursor 'word-end))
(map 'normal "E"      '(move-cursor 'big-word-end))
(map 'normal "ge"     '(move-cursor 'word-back-end))
(map 'normal "gE"     '(move-cursor 'big-word-back-end))
(map 'normal "%"      '(move-cursor 'match-bracket))
(map 'normal "<c-d>"  '(move-cursor 'half-page-down))
(map 'normal "<c-u>"  '(move-cursor 'half-page-up))
(map 'normal "zz"     '(move-cursor 'center))

;; ----- window management (<c-w> prefix) --------------------------------
(map 'normal "<c-w>q" '(window-close))
(map 'normal "<c-w>\"" '(window-split 'vertical))
(map 'normal "<c-w>|" '(window-split 'horizontal))
(map 'normal "<c-w>h" '(window-focus 'left))
(map 'normal "<c-w>l" '(window-focus 'right))
(map 'normal "<c-w>k" '(window-focus 'up))
(map 'normal "<c-w>j" '(window-focus 'down))
(map 'normal "<c-w>w" '(window-focus-next))

;; ----- visual modes ----------------------------------------------------
;; All three visual modes share the same motion set as normal mode. The
;; selection anchor is captured by `set-mode` when entering a visual mode
;; and preserved when switching between visual submodes — so `v`/`V`/`<c-v>`
;; inside a visual mode just changes the selection geometry. The motion set
;; is duplicated per mode because rizz has no implicit sequencing form.

;; visual (characterwise)
(fn bind-visual-motions (mode)
    (do
      (map mode ":"        '(set-mode 'command))
      (map mode "<esc>"    '(set-mode 'normal))
      (map mode "v"        '(set-mode 'normal))
      (map mode "V"        '(set-mode 'visual-line))
      (map mode "<c-v>"    '(set-mode 'visual-block))
      (map mode "j"        '(move-cursor 'down))
      (map mode "<down>"   '(move-cursor 'down))
      (map mode "k"        '(move-cursor 'up))
      (map mode "<up>"     '(move-cursor 'up))
      (map mode "h"        '(move-cursor 'left))
      (map mode "<left>"   '(move-cursor 'left))
      (map mode "l"        '(move-cursor 'right))
      (map mode "<right>"  '(move-cursor 'right))
      (map mode "0"        '(move-cursor 'line-start))
      (map mode "^"        '(move-cursor 'line-first-non-blank))
      (map mode "$"        '(move-cursor 'line-end))
      (map mode "gg"       '(move-cursor 'file-start))
      (map mode "G"        '(move-cursor 'file-end))
      (map mode "b"        '(move-cursor 'word-start))
      (map mode "B"        '(move-cursor 'big-word-start))
      (map mode "w"        '(move-cursor 'word-forward))
      (map mode "W"        '(move-cursor 'big-word-forward))
      (map mode "e"        '(move-cursor 'word-end))
      (map mode "E"        '(move-cursor 'big-word-end))
      (map mode "ge"       '(move-cursor 'word-back-end))
      (map mode "gE"       '(move-cursor 'big-word-back-end))
      (map mode "%"        '(move-cursor 'match-bracket))
      (map mode "<c-d>"    '(move-cursor 'half-page-down))
      (map mode "<c-u>"    '(move-cursor 'half-page-up))
      (map mode "zz"       '(move-cursor 'center))
      (map mode "x"        '(delete-selection))
      (map mode "d"        '(delete-selection))))

(bind-visual-motions 'visual)
(bind-visual-motions 'visual-block)
(bind-visual-motions 'visual-line)

;; ----- insert mode -----------------------------------------------------
(map 'insert "<enter>"     '(newline))
(map 'insert "<backspace>" '(delete-char))
(map 'insert "<esc>"       '(set-mode 'normal))
(map 'insert "jk"          '(set-mode 'normal))
(map 'insert "<up>"        '(move-cursor 'up))
(map 'insert "<down>"      '(move-cursor 'down))
(map 'insert "<left>"      '(move-cursor 'left))
(map 'insert "<right>"     '(move-cursor 'right))

;; ----- command mode (minibuffer) ---------------------------------------
(map 'command "<enter>"     '(command-submit))
(map 'command "<backspace>" '(delete-char))
(map 'command "<esc>"       '(command-cancel))
(map 'command "<left>"      '(move-cursor 'left))
(map 'command "<right>"     '(move-cursor 'right))

;; ----- notification popup ----------------------------------------------
;; `notify` is the bridge between Rust-side notifications (eval errors,
;; render-callback failures, command-submit results) and the user-visible
;; popup. The popup itself is constructed here — placement, chrome, faces,
;; and dedup all live in lisp — so swap any of this in `init.lisp` to
;; restyle messages without touching the editor binary.
;;
;; Two collaborators in Rust make this possible:
;;   * `notify-record` appends `msg` to the history vector (still owned by
;;     `State` so `:messages` / `(message-history)` stay coherent).
;;   * `popup-mode` reports the keymap mode of the topmost popup so we can
;;     refill an existing notify popup instead of stacking a new one.

;; Short messages render as virtual text in the minibuffer strip; anything
;; over `_notify-popup-threshold` chars opens the centered popup instead.
;; The minibuffer auto-clears when the user enters command mode (`:`), so
;; virtual text is wiped automatically the moment they start typing.
;;
;; Both branches end with `()` (unit) so `command-submit` / `evaluate` don't
;; surface their return value (the popup bufno from `popup-open`, or the
;; unit from `buf-text-set`) — that would recursively call `notify` on the
;; return value and clobber what we just rendered.
(let _notify-popup-threshold 80)

(fn notify (msg . args)
  (do
    (notify-record msg)
    (let args (car args))
    (let! force ())
    (let! title " message — q/<esc>/<enter> to dismiss ")
    (if (= 'map (typeof args))
      (do (set! force (get args "force"))

          (if (get args "title")
              (set! title (get args "title"))
              ())
        )
      ())
    (if (or (deref force) (> (len msg) _notify-popup-threshold))
            (if (= (popup-mode) "popup")
                (buf-text-set (popup-bufno) msg)
                (popup-open
                  (block (buffer-view)
                    {"border":      "plain"
                     "title":       (deref title)
                     "face":        "popup.default"
                     "border-face": "popup.border"
                     "title-face":  "popup.title"})
                  {"text":      msg
                   "modes":     ['popup]
                   "placement": {"kind": "center" "w": 0.6 "h": 0.6}
                   "wrap-mode": 'word}))
        (buf-text-set (minibuffer-bufno) msg))
      ()))

;; `:messages` — open the popup with the full notification history. Same
;; chrome as `notify`, but seeded with every recorded message instead of
;; just the latest.
(fn messages ()
  (do
    (fn _row (i line) (str-join [(to-str i) line] ". "))
    (let rows (fmapi _row (message-history)))
    (popup-open
      (block (buffer-view)
        {"border":      "plain"
         "title":       " messages — q/<esc>/<enter> to dismiss "
         "face":        "popup.default"
         "border-face": "popup.border"
         "title-face":  "popup.title"})
      {"text":      (str-join rows "\n")
       "modes":     ['popup]
       "placement": {"kind": "center" "w": 0.6 "h": 0.6}})
    ()))


(fn history ()
  (do
    (fn _row (i line) (str-join [(to-str (+ 1 i)) line] ". "))
    (let rows (fmapi _row (command-history)))
    (popup-open
      (block (buffer-view)
        {"border":      "rounded"
         "title":       " command history — q/<esc>/<enter> to dismiss "
         "face":        "popup.default"
         "border-face": "popup.border"
         "title-face":  "popup.title"})
      {"text":      (str-join rows "\n")
       "modes":     ['popup]
       "placement": {"kind": "side" "side": "bottom" "size": (clamp (len rows) 5 50)}})
    ()))

;; ----- popup mode ------------------------------------------------------
;; The default keymap a popup is interpreted under. While a popup is on top
;; of the stack `handle_key_event` resolves keys against the popup's
;; `keymap_mode` instead of the focused buffer's editing mode — so this
;; layer is what gives the message popup (and any other popup that doesn't
;; choose a custom mode) its scroll / dismiss behavior.
;;
;; Custom popups can declare their own mode (e.g. `"popup.files"`) and bind
;; whatever keys they like; the popup's backing buffer participates in the
;; normal editor primitives (move-cursor, insert-char, …) under that mode.
(map 'popup "j"          '(move-cursor 'down))
(map 'popup "<down>"     '(move-cursor 'down))
(map 'popup "k"          '(move-cursor 'up))
(map 'popup "<up>"       '(move-cursor 'up))
(map 'popup "h"          '(move-cursor 'left))
(map 'popup "<left>"     '(move-cursor 'left))
(map 'popup "l"          '(move-cursor 'right))
(map 'popup "<right>"    '(move-cursor 'right))
(map 'popup "<c-d>"      '(move-cursor 'half-page-down))
(map 'popup "<c-u>"      '(move-cursor 'half-page-up))
(map 'popup "<pagedown>" '(move-cursor 'half-page-down))
(map 'popup "<pageup>"   '(move-cursor 'half-page-up))
(map 'popup "gg"         '(move-cursor 'file-start))
(map 'popup "G"          '(move-cursor 'file-end))
(map 'popup "0"          '(move-cursor 'line-start))
(map 'popup "$"          '(move-cursor 'line-end))
(map 'popup "q"          '(popup-close))
(map 'popup "<esc>"      '(popup-close))
(map 'popup "<enter>"    '(popup-close))

;; --- (popup-files)  centered file-explorer popup -----------------------
;; Reuses the buffer machinery (the popup buffer holds the directory
;; listing as plain text) and *layers* `popup.files` on top of `popup`
;; so that only the explorer-specific bindings (open / parent dir) need
;; to be defined here — j/k/q/<esc>/movement keys are inherited from the
;; base `popup` keymap by the layered resolver.

;; Snapshot of the directory we listed, indexed by line number. Updated on
;; each `(popup-files)` invocation so `<enter>` knows which path the cursor
;; is on without parsing the buffer text back.
(let _popup-files-entries (ref []))

(fn _popup-files-render (dir)
  (do
    (let dir
      (if (fs-isdir dir)
          dir
          (fs-parent dir)))

    (let entries (fs-readdir dir))
    (set! _popup-files-entries entries)
    (str-join
      (fmap (fn _row (p) (to-str p)) entries)
      "\n")))

(fn popup-files ()
  (do
    (let dir (if (buf-path)
                 (buf-path)
                 (workdir)))
    (popup-open
      (block (buffer-view)
        {"border":      "rounded"
         "title":       (str-join [" files: " (to-str dir) " "] "")
         "face":        "popup.default"
         "border-face": "popup.border"
         "title-face":  "popup.title"})
      {"text":        (_popup-files-render dir)
       "modes":       ['popup 'popup.files]
       "buffer-mode": 'normal
       "placement":   'full
       "show-cursor": 1})))

;; Bind the explorer to `<space>f`. Motion and dismiss keys are inherited
;; from the `popup` layer beneath `popup.files`, so only the explorer-
;; specific actions (open + parent-dir) need to be bound here.
(keymap-set 'normal "<c-e>" '(popup-files))

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
            (block (buffer-view)
              {"border":      "rounded"
               "title":       (str-join [" files: " target " "] "")
               "face":        "popup.default"
               "border-face": "popup.border"
               "title-face":  "popup.title"})
            {"text":        (_popup-files-render target)
             "mode":        'popup.files
             "buffer-mode": 'normal
             "placement":   "full"
             "show-cursor": 1})
     (edit target)
     ))
))


(keymap-set 'popup.files "-"
   '(do (let entries (deref _popup-files-entries))
        (let target (fs-parent (fs-parent (first entries))))
        (popup-close)
        (if (= target ())
          ()
        (if (fs-isdir target)
          (popup-open
            (block (buffer-view)
              {"border":      "rounded"
               "title":       (str-join [" files: " target " "] "")
               "face":        "popup.default"
               "border-face": "popup.border"
               "title-face":  "popup.title"})
            {"text":        (_popup-files-render target)
             "mode":        'popup.files
             "buffer-mode": 'normal
             "placement":   "full"
             "show-cursor": 1})
          ()
        ))
     ))

(map 'popup.files "<c-e>" '(popup-close))
