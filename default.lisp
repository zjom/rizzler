;; Default keybindings, bundled into the editor binary and evaluated by
;; `State::with_config` before any user `init.lisp`. Mirrors the layout that
;; used to live in `src/keymap/default.rs`.

;; ----- normal mode -----------------------------------------------------
(keymap-set 'normal ":"      '(set-mode 'command))
(keymap-set 'normal "i"      '(set-mode 'insert))

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
