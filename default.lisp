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
(map 'normal "j"      '(move-cursor 'down))
(map 'normal "<down>" '(move-cursor 'down))
(map 'normal "k"      '(move-cursor 'up))
(map 'normal "<up>"   '(move-cursor 'up))
(map 'normal "h"      '(move-cursor 'left))
(map 'normal "<left>" '(move-cursor 'left))
(map 'normal "l"      '(move-cursor 'right))
(map 'normal "<right>" '(move-cursor 'right))

(map 'normal "0"      '(move-cursor 'line-start))
(map 'normal "$"      '(move-cursor 'line-end))
(map 'normal "gg"     '(move-cursor 'file-start))
(map 'normal "G"      '(move-cursor 'file-end))
(map 'normal "b"      '(move-cursor 'word-start))
(map 'normal "e"      '(move-cursor 'word-end))
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
      (map mode "$"        '(move-cursor 'line-end))
      (map mode "gg"       '(move-cursor 'file-start))
      (map mode "G"        '(move-cursor 'file-end))
      (map mode "b"        '(move-cursor 'word-start))
      (map mode "e"        '(move-cursor 'word-end))
      (map mode "<c-d>"    '(move-cursor 'half-page-down))
      (map mode "<c-u>"    '(move-cursor 'half-page-up))
      (map mode "zz"       '(move-cursor 'center))))

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
