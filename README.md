# Lsp Client

Language server protocol client implement in Rust.
First target editor is neovim.
First target language server is [rust-analyzer](https://github.com/rust-analyzer/rust-analyzer)

# Development setup

1. Clone project
2. Add path to vim-plug:
```
Plug '~/path/to/lspc'
```

3. Build project
```
cargo build
```

4. Config your vim:
```
let g:lspc = {
      \ 'rust': {
      \     'root_markers': ['Cargo.lock'],
      \     'command': ['rustup', 'run', 'stable', 'ra_lsp_server'],
      \     },
      \ }
```

5. Start Rust handler:
```
:LspcStart
```
or
```
:call lspc#init()
```

6. Test command:
```
:call lspc#hello_from_the_other_side()
```

7. Start Language Server for current buffer
```
:call lspc#start_lang_server()
```

8. View debug log at `log.txt`

