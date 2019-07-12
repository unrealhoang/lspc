function! lspc#buffer#filename() abort
    " When executing autocommand, `%` might have already changed.
    let l:filename = expand('<afile>:p')
    if !l:filename
        let l:filename = expand('%:p')
    endif
    return l:filename
endfunction

" This function will return buffer text as required by LSP.
"
" The main difference with getbufline is that it checks fixendofline settings
" and add extra line at ending if appropriate.
function! lspc#buffer#text(...) abort
    let l:buf = get(a:000, 0, '')

    let l:lines = getbufline(l:buf, 1, '$')
    if len(l:lines) > 0 && l:lines[-1] !=# '' && &fixendofline
        let l:lines += ['']
    endif
    return l:lines
endfunction

function! lspc#buffer#line() abort
    return line('.') - 1
endfunction

function! lspc#buffer#character() abort
    return col('.') - 1
endfunction

function! lspc#buffer#range_start_line() abort
    let l:lnum = v:lnum ? v:lnum : getpos("'<")[1]
    return l:lnum - 1
endfunction

function! lspc#buffer#range_end_line() abort
    if v:lnum
        return v:lnum - 1 + v:count
    endif

    return getpos("'>")[1]
endfunction

function! lspc#buffer#viewport() abort
    return {
        \ 'start': line('w0') - 1,
        \ 'end': line('w$'),
        \ }
endfunction

function! lspc#buffer#position() abort
	return {
		\ 'line': lspc#buffer#line(),
		\ 'character': lspc#buffer#character(),
		\ }
endfunction
