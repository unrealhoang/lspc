" Commands
command! -nargs=0 LspcStart call lspc#init()

augroup lspc
  autocmd!
  autocmd BufNewFile,BufRead * call lspc#did_open()
augroup END
