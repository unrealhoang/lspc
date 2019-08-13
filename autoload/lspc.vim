function! lspc#config()

endfunction

let s:root = expand('<sfile>:p:h:h')

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
  let l:binpath = s:root . '/target/release/neovim_lspc'

  let s:job_id = jobstart([l:binpath], {
        \ 'rpc': v:true,
        \ 'on_stderr': function('s:echo_handler'),
        \ 'on_exit':   function('s:echo_handler'),
        \ })
  if s:job_id == -1
    echo "[LSPC] Cannot execute " . l:binpath
  endif
endfunction

function! lspc#started() abort
  return exists('s:job_id')
endfunction

function! lspc#destroy()
  call jobstop(s:job_id)
endfunction

function! lspc#current_file_type()

endfunction

function! lspc#start_lang_server()
  let l:lang_id = 'rust'
  let l:config = g:lspc[l:lang_id]
  let l:cur_path = lspc#buffer#filename()
  call rpcnotify(s:job_id, 'start_lang_server', l:lang_id, l:config, l:cur_path)
endfunction

function! lspc#hover()
  let l:lang_id = 'rust'
  let l:cur_path = lspc#buffer#filename()
  let l:position = lspc#buffer#position()
  call rpcnotify(s:job_id, 'hover', l:lang_id, l:cur_path, l:position)
endfunction

function! lspc#did_open()
  let l:lang_id = 'rust'
  let l:cur_path = lspc#buffer#filename()
  call rpcnotify(s:job_id, 'did_open', l:cur_path)
endfunction

function! lspc#goto_definition()
  let l:lang_id = 'rust'
  let l:cur_path = lspc#buffer#filename()
  let l:position = lspc#buffer#position()
  call rpcnotify(s:job_id, 'goto_definition', l:lang_id, l:cur_path, l:position)
endfunction

function! lspc#inlay_hints()
  let l:lang_id = 'rust'
  let l:cur_path = lspc#buffer#filename()
  call rpcnotify(s:job_id, 'inlay_hints', l:lang_id, l:cur_path)
endfunction

function! lspc#format_doc()
  let l:lang_id = 'rust'
  let l:cur_path = lspc#buffer#filename()
  let l:lines = lspc#buffer#text()
  call rpcnotify(s:job_id, 'format_doc', l:lang_id, l:cur_path, l:lines)
endfunction

function! lspc#hello_from_the_other_side()
  call rpcnotify(s:job_id, 'hello')
endfunction

function! lspc#debug()
  echo "Output Buffer: " . s:output_buffer
endfunction
