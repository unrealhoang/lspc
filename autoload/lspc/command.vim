let s:FLOAT_WINDOW_AVAILABLE = has('nvim') && exists('*nvim_open_win')

function! lspc#command#close_floatwin_on_cursor_move(win_id, opened) abort
    if getpos('.') == a:opened
        " Just after opening floating window, CursorMoved event is run.
        " To avoid closing floating window immediately, check the cursor
        " was really moved
        return
    endif
    autocmd! plugin-lspc-close-hover
    let winnr = win_id2win(a:win_id)
    if winnr == 0
        return
    endif
    execute winnr . 'wincmd c'
endfunction

function! lspc#command#close_floatwin_on_buf_enter(win_id, bufnr) abort
    let winnr = win_id2win(a:win_id)
    if winnr == 0
        " Float window was already closed
        autocmd! plugin-lspc-close-hover
        return
    endif
    if winnr == winnr()
        " Cursor is moving into floating window. Do not close it
        return
    endif
    if bufnr('%') == a:bufnr
        " When current buffer opened hover window, it's not another buffer. Skipped
        return
    endif
    autocmd! plugin-lspc-close-hover
    execute winnr . 'wincmd c'
endfunction

" Open preview window. Window is open in:
"   - Floating window on Neovim (0.4.0 or later)
"   - Preview window on Neovim (0.3.0 or earlier) or Vim
function! lspc#command#open_hover_preview(bufname, lines, filetype) abort
    " Use local variable since parameter is not modifiable
    let lines = a:lines
    let bufnr = bufnr('%')

    let use_float_win = s:FLOAT_WINDOW_AVAILABLE
    if use_float_win
        let pos = getpos('.')

        " Calculate width and height and give margin to lines
        let width = 0
        for index in range(len(lines))
            let line = lines[index]
            if line !=# ''
                " Give a left margin
                let line = ' ' . line
            endif
            let lw = strdisplaywidth(line)
            if lw > width
                let width = lw
            endif
            let lines[index] = line
        endfor

        " Give margin
        let width += 1
        let lines = [''] + lines + ['']
        let height = len(lines)

        " Calculate anchor
        " Prefer North, but if there is no space, fallback into South
        let bottom_line = line('w0') + winheight(0) - 1
        if pos[1] + height <= bottom_line
            let vert = 'N'
            let row = 1
        else
            let vert = 'S'
            let row = 0
        endif

        " Prefer West, but if there is no space, fallback into East
        if pos[2] + width <= &columns
            let hor = 'W'
            let col = 0
        else
            let hor = 'E'
            let col = 1
        endif

        let float_win_id = nvim_open_win(bufnr, v:true, {
        \   'relative': 'cursor',
        \   'anchor': vert . hor,
        \   'row': row,
        \   'col': col,
        \   'width': width,
        \   'height': height,
        \ })

        execute 'noswapfile edit!' a:bufname

        setlocal winhl=Normal:CursorLine
    else
        execute 'silent! noswapfile pedit!' a:bufname
        wincmd P
    endif

    setlocal buftype=nofile nobuflisted bufhidden=wipe nonumber norelativenumber signcolumn=no modifiable

    if a:filetype isnot v:null
        let &filetype = a:filetype
    endif

    call setline(1, lines)
    setlocal nomodified nomodifiable

    wincmd p

    if use_float_win
        " Unlike preview window, :pclose does not close window. Instead, close
        " hover window automatically when cursor is moved.
        let call_after_move = printf('lspc#command#close_floatwin_on_cursor_move(%d, %s)', float_win_id, string(pos))
        let call_on_bufenter = printf('lspc#command#close_floatwin_on_buf_enter(%d, %d)', float_win_id, bufnr)
        augroup plugin-lspc-close-hover
            execute 'autocmd CursorMoved,CursorMovedI,InsertEnter <buffer> call ' . call_after_move
            execute 'autocmd BufEnter * call ' . call_on_bufenter
        augroup END
    endif
endfunction

" Used for prevent select on unwanted line in reference buffer
function! s:reference_prevent_touch_line(untouch_lines)
  let l:line = line('.')
  while index(a:untouch_lines, l:line) >= 0
    let l:line = l:line + 1
    call cursor(col('.'), l:line)
  endwhile
endfunction
" Open reference window. Window is open in:
"   - Floating window on Neovim (0.4.0 or later)
"   - Preview window on Neovim (0.3.0 or earlier) or Vim
function! lspc#command#open_reference_preview(bufname, lines) abort
  let lines = a:lines
  let height = float2nr((&lines - 2) * 0.6) " lightline + status
  let row = float2nr((&lines - height) / 2)
  let width = float2nr(&columns * 0.3)
  let col = 0
  let opts = {
      \ 'relative': 'editor',
      \ 'row': row,
      \ 'col': col,
      \ 'width': width,
      \ 'height': height
      \ }

  let buf = nvim_create_buf(v:false, v:true)
  let win = nvim_open_win(buf, v:true, opts)

  execute 'noswapfile edit!' a:bufname
  setlocal
    \ buftype=nofile
    \ nobuflisted
    \ nonumber
    \ bufhidden=hide
    \ norelativenumber
    \ signcolumn=no
    \ cursorline
    \ cc=

  let s:untouch_lines = [1]
  let reference_count = len(lines)
  call setline(1, reference_count . ' reference' . (reference_count > 1 ? 's' : ''))
  call setline(2, lines)
  call s:reference_prevent_touch_line(s:untouch_lines)

  augroup plugin-lspc-reference control
    autocmd CursorMoved <buffer> call s:reference_prevent_touch_line(s:untouch_lines)
  augroup END
endfunction
