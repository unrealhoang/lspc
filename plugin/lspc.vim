" Commands
command! -nargs=0 LspcStart call lspc#init()

augroup lspc
  autocmd!
  if !lspc#started()
    autocmd VimEnter         * call lspc#init()
  endif
  autocmd BufNewFile,BufRead * call lspc#did_open()
  autocmd VimLeave           * call lspc#destroy()
augroup END
