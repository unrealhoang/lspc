let s:config = {
      \ 'auto_start': v:true,
      \ }

function! lspc#output(log)
  " if !exists('s:output_buffer') || !nvim_buf_is_loaded(s:output_buffer)
  "   let s:output_buffer = nvim_create_buf(v:true, v:false)
  "   call nvim_buf_set_name(s:output_buffer, "[LSPC Output]")
  "   call nvim_buf_set_option(s:output_buffer, "buftype", "nofile")
  "   call nvim_buf_set_option(s:output_buffer, "buflisted", v:true)
  " endif

  " let l:line = nvim_buf_line_count(s:output_buffer) - 1
  " call nvim_buf_set_lines(s:output_buffer, l:line, l:line, v:true, a:log)
  echom string(a:log)
endfunction

function! s:echo_handler(job_id, data, name)
  let l:log = "[LSPC][" . a:name ."]. Data: " . string(a:data)
  call lspc#output(l:log)
endfunction

function! lspc#init()
  call extend(s:config, g:lspc)

  if lspc#started()
    return
  endif

  let s:lang_servers = []

  let l:binpath = s:root . '/target/debug/neovim_lspc'

  call setenv('RUST_BACKTRACE', '1')
  let s:job_id = jobstart([l:binpath], {
        \ 'rpc': v:true,
        \ 'on_stderr': function('s:echo_handler'),
        \ 'on_exit':   function('s:echo_handler'),
        \ })
  if s:job_id == -1
    echom "[LSPC] Cannot execute " . l:binpath
  endif
endfunction

function! lspc#started() abort
  return exists('s:job_id')
endfunction

function! lspc#destroy()
  call jobstop(s:job_id)
  unlet! s:job_id
endfunction

function! lspc#start_lang_server()
  if exists('b:current_syntax')
    let l:lang_id = b:current_syntax
    if has_key(s:config, l:lang_id) && !lspc#lang_server_started(l:lang_id)
      let l:config = s:config[l:lang_id]
      let l:cur_path = lspc#buffer#filename()
      call add(s:lang_servers, l:lang_id)
      call rpcnotify(s:job_id, 'start_lang_server', l:lang_id, l:config, l:cur_path)
    endif
  endif
endfunction

function! lspc#lang_server_started(lang_id)
  return index(s:lang_servers, a:lang_id) >= 0
endfunction

function! lspc#hover()
  let l:buf_id = bufnr()
  let l:cur_path = lspc#buffer#filename()
  let l:position = lspc#buffer#position()
  call rpcnotify(s:job_id, 'hover', l:buf_id, l:cur_path, l:position)
endfunction

function! lspc#reference()
  let l:buf_id = bufnr()
  let l:cur_path = lspc#buffer#filename()
  let l:position = lspc#buffer#position()
  let l:include_declaration = v:false
  call rpcnotify(s:job_id, 'references', l:buf_id, l:cur_path, l:position, l:include_declaration)
endfunction

function! lspc#track_all_buffers()
  let l:all_buffers = range(1, bufnr('$'))
  let l:listed_buffers = filter(l:all_buffers, 'buflisted(v:val)')
  for l:buf_id in listed_buffers
    let l:buf_path = expand('#' . buf_id . ':p')
    call rpcnotify(s:job_id, 'did_open', l:buf_id, l:buf_path)
  endfor
endfunction

function! lspc#did_open()
  let l:buf_id = bufnr()
  let l:cur_path = lspc#buffer#filename()
  if s:config['auto_start']
    call lspc#start_lang_server()
  endif
  call rpcnotify(s:job_id, 'did_open', l:buf_id, l:cur_path)
endfunction

function! lspc#goto_definition()
  let l:buf_id = bufnr()
  let l:cur_path = lspc#buffer#filename()
  let l:position = lspc#buffer#position()
  call rpcnotify(s:job_id, 'goto_definition', l:buf_id, l:cur_path, l:position)
endfunction

function! lspc#inlay_hints()
  let l:buf_id = bufnr()
  let l:cur_path = lspc#buffer#filename()
  call rpcnotify(s:job_id, 'inlay_hints', l:buf_id, l:cur_path)
endfunction

function! lspc#format_doc()
  let l:buf_id = bufnr()
  let l:cur_path = lspc#buffer#filename()
  let l:lines = lspc#buffer#text()
  call rpcnotify(s:job_id, 'format_doc', l:buf_id, l:cur_path, l:lines)
endfunction

function! lspc#hello_from_the_other_side()
  call rpcnotify(s:job_id, 'hello')
endfunction

function! lspc#debug()
  echo "Output Buffer: " . s:output_buffer
endfunction

let s:root = expand('<sfile>:p:h:h')

if !exists('s:job_id')
  call lspc#init()
endif
