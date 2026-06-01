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
(fn bind-visual-motions (mode)
    (do
      (keymap-set mode "<esc>"    '(set-mode 'normal))
      (keymap-set mode "v"        '(set-mode 'normal))
      (keymap-set mode "V"        '(set-mode 'visual-line))
      (keymap-set mode "<c-v>"    '(set-mode 'visual-block))
      (keymap-set mode "j"        '(move-cursor 'down))
      (keymap-set mode "<down>"   '(move-cursor 'down))
      (keymap-set mode "k"        '(move-cursor 'up))
      (keymap-set mode "<up>"     '(move-cursor 'up))
      (keymap-set mode "h"        '(move-cursor 'left))
      (keymap-set mode "<left>"   '(move-cursor 'left))
      (keymap-set mode "l"        '(move-cursor 'right))
      (keymap-set mode "<right>"  '(move-cursor 'right))
      (keymap-set mode "0"        '(move-cursor 'line-start))
      (keymap-set mode "$"        '(move-cursor 'line-end))
      (keymap-set mode "gg"       '(move-cursor 'file-start))
      (keymap-set mode "G"        '(move-cursor 'file-end))
      (keymap-set mode "b"        '(move-cursor 'word-start))
      (keymap-set mode "e"        '(move-cursor 'word-end))
      (keymap-set mode "<c-d>"    '(move-cursor 'half-page-down))
      (keymap-set mode "<c-u>"    '(move-cursor 'half-page-up))
      (keymap-set mode "zz"       '(move-cursor 'center))))

(bind-visual-motions 'visual)
(bind-visual-motions 'visual-block)
(bind-visual-motions 'visual-line)

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
