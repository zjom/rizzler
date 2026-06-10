# Features

- [ ] `<up>`/ `down` to autofill command history
- [ ] lifecycle hooks (autocommands)
  - see vim help autocmd
- [ ] named mutable widgets
- [ ] lspconfig
- [ ] add (non-lsp) formatter support
- [ ] shiftwidth/ expandtab/ tab-width config
- [ ] highlight matching pair (parens, brackets, etc)
- [ ] picker (telescope like)
- [ ] add support for package manager installation of lsps, formatters, linters (look at what mason.nvim does)
- [ ] sed
- [ ] multicursor editing

# Improvements

- [ ] add more known tree-sitter languages to grammars.toml
- [ ] add more known lsps to lsp.toml
- [ ] support more default tree-sitter highlights
- [ ] filetype on buffers
- [ ] buffers cycle should by access order instead of creation order
- [ ] improve latency of lsp completion menu
- [ ] better tracing/ error handling/ reporting
- [ ] split default init.rz into multiple files and seed folder instead of single file.

# Bugs

- [ ] user defined functions not showing in completion menu
- [ ] cursor-* reporting is not tied to editor window
  - [ ] moving cursor in command buffer and popups updates cursor-line of editor
  - [ ] gutter shifts when entering command buffer
