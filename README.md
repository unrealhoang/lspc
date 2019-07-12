# Lsp Client

Language server protocol client implement in Rust.
First target editor is neovim.

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

4. Start Rust handler:
```
:LspcStart
```
or
```
:call lspc#init()
```

5. Test command:
```
:call lspc#hello_from_the_other_side()
```

