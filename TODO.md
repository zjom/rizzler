# Features

- [ ] lifecycle hooks (autocommands)
  - see vim help autocmd
- [ ] widgets
  - [ ] named mutable widgets
  - [ ] declarative widget api
  - [ ] more prebuilt widgets for common things
- [ ] lspconfig
- [ ] add (non-lsp) formatter support
- [ ] editor options and buffer options
  - [ ] shiftwidth/ expandtab/ tab-width config
  - [ ] line wrap
- [ ] highlight matching pair (parens, brackets, etc)
- [ ] add support for package manager installation of lsps, formatters, linters (look at what mason.nvim does)
- [ ] sed
- [ ] multicursor editing

# Improvements

- [x] quit enhancements
  - [x] to close current buffer (unless last one)
  - [x] quit-all to exit
  - [x] quit-force to exit without flushing to disk
  - [x] prevent quit when buffer has changes not flushed
- [ ] add more known tree-sitter languages to grammars.toml
- [ ] add more known lsps to lsp.toml
- [ ] support more default tree-sitter highlights
- [ ] /after/filetype
- [ ] buffers cycle should by access order instead of creation order
- [ ] better tracing/ error handling/ reporting
- [ ] split default init.rz into multiple files and seed folder instead of single file.

# Bugs

- [ ] cursor clamped to row length instead of min(row length, buffer width)
  - open vertical split
  - type line longer than buffer width
  - cursor reaches into other buffer

