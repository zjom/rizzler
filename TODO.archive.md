# Features

- [x] `<up>`/ `down` to autofill command history
- [x] add `<<` and `>>` like vim
- [x] picker (telescope like)

# Improvements

- [x] clean up comments and architecture
- [x] documentation for builtins
- [x] `o`/`O`/`enter` should be whitespace aware. put you on col of start of prev line
- [x] improve latency of lsp completion menu
- [x] (2026-06-18) quit enhancements
  - [x] to close current buffer (unless last one)
  - [x] quit-all to exit
  - [x] quit-force to exit without flushing to disk
  - [x] prevent quit when buffer has changes not flushed

# Bugs

- [x] user defined functions not showing in completion menu
- [x] cursor-* reporting is not tied to editor window
  - [x] moving cursor in command buffer and popups updates cursor-line of editor
  - [x] gutter shifts when entering command buffer
