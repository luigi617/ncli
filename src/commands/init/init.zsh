zmodload zsh/system zsh/zselect
zmodload -F zsh/files b:zf_mv b:zf_rm

typeset -g _ncli_capture=NCLI_CAPTURE
typeset -g _ncli_recording=NCLI_RECORDING
typeset -gi _ncli_recordings
typeset -gaU ncli_captured
typeset -ga _ncli_separators=(';' '|' '||' '&' '&&' '|&' '&!' '&|')

_ncli_preexec() {
  local -a words=(${(z)1})
  local -a separators=(${words:*_ncli_separators})
  (( $#separators )) && return
  local word fd
  for word in $words; do
    [[ $word == (sudo|env|command|builtin|exec|nocorrect|noglob|nohup|time|-*|*=*) ]] || break
  done
  (( ${ncli_captured[(Ie)${word:t}]} )) || return
  typeset -g _ncli_partial=$_ncli_capture.$$.$(( ++_ncli_recordings ))
  sysopen -w -o creat,excl -m 600 -u fd $_ncli_partial || return
  exec {fd}>&-
  sysopen -w -o creat,trunc -m 600 -u fd $_ncli_recording || { zf_rm -f -- $_ncli_partial; return }
  print -r -u $fd -- $_ncli_partial
  exec {fd}>&- {_ncli_stdout}>&1 {_ncli_stderr}>&2
  exec > >(tee -i -- $_ncli_partial; _ncli_publish $_ncli_partial) 2>&1
}

_ncli_publish() {
  if [[ $(<$_ncli_recording) == $1 ]]; then
    zf_mv -f -- $1 $_ncli_capture
  else
    zf_rm -f -- $1
  fi
} 2>/dev/null

_ncli_precmd() {
  (( ${+_ncli_stdout} )) || return
  exec >&$_ncli_stdout 2>&$_ncli_stderr {_ncli_stdout}>&- {_ncli_stderr}>&-
  unset _ncli_stdout _ncli_stderr
  local -i waited
  while [[ -e $_ncli_partial ]] && (( waited++ < 100 )); do
    zselect -t 1
  done
  unset _ncli_partial
}

autoload -Uz add-zsh-hook
add-zsh-hook preexec _ncli_preexec
add-zsh-hook precmd _ncli_precmd
