;; Default keybindings, bundled into the editor binary and evaluated by
;; `State::with_config` before any user `init.lisp`. Mirrors the layout that
;; used to live in `src/keymap/default.rs`.

;; ----- normal mode -----------------------------------------------------
(keymap-set 'normal ":"      '(set-mode 'command))
(keymap-set 'normal "i"      '(set-mode 'insert))
(keymap-set 'normal "v"      '(set-mode 'visual))
(keymap-set 'normal "V"      '(set-mode 'visual-line))
(keymap-set 'normal "<c-v>"  '(set-mode 'visual-block))

(keymap-set 'normal "j"      '(move-cursor 'down))
(keymap-set 'normal "<down>" '(move-cursor 'down))
(keymap-set 'normal "k"      '(move-cursor 'up))
(keymap-set 'normal "<up>"   '(move-cursor 'up))
(keymap-set 'normal "h"      '(move-cursor 'left))
(keymap-set 'normal "<left>" '(move-cursor 'left))
(keymap-set 'normal "l"      '(move-cursor 'right))
(keymap-set 'normal "<right>" '(move-cursor 'right))

(keymap-set 'normal "0"      '(move-cursor 'line-start))
(keymap-set 'normal "$"      '(move-cursor 'line-end))
(keymap-set 'normal "gg"     '(move-cursor 'file-start))
(keymap-set 'normal "G"      '(move-cursor 'file-end))
(keymap-set 'normal "b"      '(move-cursor 'word-start))
(keymap-set 'normal "e"      '(move-cursor 'word-end))
(keymap-set 'normal "<c-d>"  '(move-cursor 'half-page-down))
(keymap-set 'normal "<c-u>"  '(move-cursor 'half-page-up))
(keymap-set 'normal "zz"     '(move-cursor 'center))

;; ----- window management (<c-w> prefix) --------------------------------
(keymap-set 'normal "<c-w>q" '(window-close))
(keymap-set 'normal "<c-w>\"" '(window-split 'vertical))
(keymap-set 'normal "<c-w>|" '(window-split 'horizontal))
(keymap-set 'normal "<c-w>h" '(window-focus 'left))
(keymap-set 'normal "<c-w>l" '(window-focus 'right))
(keymap-set 'normal "<c-w>k" '(window-focus 'up))
(keymap-set 'normal "<c-w>j" '(window-focus 'down))
(keymap-set 'normal "<c-w>w" '(window-focus-next))

;; ----- visual modes ----------------------------------------------------
;; All three visual modes share the same motion set as normal mode. The
;; selection anchor is captured by `set-mode` when entering a visual mode
;; and preserved when switching between visual submodes — so `v`/`V`/`<c-v>`
;; inside a visual mode just changes the selection geometry. The motion set
;; is duplicated per mode because risp has no implicit sequencing form.

;; visual (characterwise)
(keymap-set 'visual "<esc>"    '(set-mode 'normal))
(keymap-set 'visual "v"        '(set-mode 'normal))
(keymap-set 'visual "V"        '(set-mode 'visual-line))
(keymap-set 'visual "<c-v>"    '(set-mode 'visual-block))
(keymap-set 'visual "j"        '(move-cursor 'down))
(keymap-set 'visual "<down>"   '(move-cursor 'down))
(keymap-set 'visual "k"        '(move-cursor 'up))
(keymap-set 'visual "<up>"     '(move-cursor 'up))
(keymap-set 'visual "h"        '(move-cursor 'left))
(keymap-set 'visual "<left>"   '(move-cursor 'left))
(keymap-set 'visual "l"        '(move-cursor 'right))
(keymap-set 'visual "<right>"  '(move-cursor 'right))
(keymap-set 'visual "0"        '(move-cursor 'line-start))
(keymap-set 'visual "$"        '(move-cursor 'line-end))
(keymap-set 'visual "gg"       '(move-cursor 'file-start))
(keymap-set 'visual "G"        '(move-cursor 'file-end))
(keymap-set 'visual "b"        '(move-cursor 'word-start))
(keymap-set 'visual "e"        '(move-cursor 'word-end))
(keymap-set 'visual "<c-d>"    '(move-cursor 'half-page-down))
(keymap-set 'visual "<c-u>"    '(move-cursor 'half-page-up))
(keymap-set 'visual "zz"       '(move-cursor 'center))

;; visual-line
(keymap-set 'visual-line "<esc>"   '(set-mode 'normal))
(keymap-set 'visual-line "v"       '(set-mode 'visual))
(keymap-set 'visual-line "V"       '(set-mode 'normal))
(keymap-set 'visual-line "<c-v>"   '(set-mode 'visual-block))
(keymap-set 'visual-line "j"       '(move-cursor 'down))
(keymap-set 'visual-line "<down>"  '(move-cursor 'down))
(keymap-set 'visual-line "k"       '(move-cursor 'up))
(keymap-set 'visual-line "<up>"    '(move-cursor 'up))
(keymap-set 'visual-line "h"       '(move-cursor 'left))
(keymap-set 'visual-line "<left>"  '(move-cursor 'left))
(keymap-set 'visual-line "l"       '(move-cursor 'right))
(keymap-set 'visual-line "<right>" '(move-cursor 'right))
(keymap-set 'visual-line "0"       '(move-cursor 'line-start))
(keymap-set 'visual-line "$"       '(move-cursor 'line-end))
(keymap-set 'visual-line "gg"      '(move-cursor 'file-start))
(keymap-set 'visual-line "G"       '(move-cursor 'file-end))
(keymap-set 'visual-line "b"       '(move-cursor 'word-start))
(keymap-set 'visual-line "e"       '(move-cursor 'word-end))
(keymap-set 'visual-line "<c-d>"   '(move-cursor 'half-page-down))
(keymap-set 'visual-line "<c-u>"   '(move-cursor 'half-page-up))
(keymap-set 'visual-line "zz"      '(move-cursor 'center))

;; visual-block
(keymap-set 'visual-block "<esc>"   '(set-mode 'normal))
(keymap-set 'visual-block "v"       '(set-mode 'visual))
(keymap-set 'visual-block "V"       '(set-mode 'visual-line))
(keymap-set 'visual-block "<c-v>"   '(set-mode 'normal))
(keymap-set 'visual-block "j"       '(move-cursor 'down))
(keymap-set 'visual-block "<down>"  '(move-cursor 'down))
(keymap-set 'visual-block "k"       '(move-cursor 'up))
(keymap-set 'visual-block "<up>"    '(move-cursor 'up))
(keymap-set 'visual-block "h"       '(move-cursor 'left))
(keymap-set 'visual-block "<left>"  '(move-cursor 'left))
(keymap-set 'visual-block "l"       '(move-cursor 'right))
(keymap-set 'visual-block "<right>" '(move-cursor 'right))
(keymap-set 'visual-block "0"       '(move-cursor 'line-start))
(keymap-set 'visual-block "$"       '(move-cursor 'line-end))
(keymap-set 'visual-block "gg"      '(move-cursor 'file-start))
(keymap-set 'visual-block "G"       '(move-cursor 'file-end))
(keymap-set 'visual-block "b"       '(move-cursor 'word-start))
(keymap-set 'visual-block "e"       '(move-cursor 'word-end))
(keymap-set 'visual-block "<c-d>"   '(move-cursor 'half-page-down))
(keymap-set 'visual-block "<c-u>"   '(move-cursor 'half-page-up))
(keymap-set 'visual-block "zz"      '(move-cursor 'center))

;; ----- insert mode -----------------------------------------------------
(keymap-set 'insert "<enter>"     '(newline))
(keymap-set 'insert "<backspace>" '(delete-char))
(keymap-set 'insert "<esc>"       '(set-mode 'normal))

;; ----- command mode (minibuffer) ---------------------------------------
(keymap-set 'command "<enter>"     '(command-submit))
(keymap-set 'command "<backspace>" '(delete-char))
(keymap-set 'command "<esc>"       '(command-cancel))
(keymap-set 'command "<left>"      '(move-cursor 'left))
(keymap-set 'command "<right>"     '(move-cursor 'right))
