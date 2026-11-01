zmodload zsh/system zsh/zselect
zmodload -F zsh/files b:zf_mv b:zf_rm

typeset -g _ncli_capture=NCLI_CAPTURE
typeset -gi _ncli_recordings
typeset -gaU ncli_captured

_ncli_preexec() {
  local -a words=(${(z)1})
  local word fd
  for word in $words; do
    [[ $word == (sudo|env|command|builtin|exec|nocorrect|noglob|nohup|time|-*|*=*) ]] || break
  done
  (( ${ncli_captured[(Ie)${word:t}]} )) || return
  typeset -g _ncli_partial=$_ncli_capture.$$.$(( ++_ncli_recordings ))
  sysopen -w -o creat,excl -m 600 -u fd $_ncli_partial || return
  exec {fd}>&- {_ncli_stdout}>&1 {_ncli_stderr}>&2
  exec > >(tee -i -- $_ncli_partial; zf_mv -f -- $_ncli_partial $_ncli_capture 2>/dev/null) 2>&1
}

_ncli_precmd() {
  (( ${+_ncli_stdout} )) || return
  exec >&$_ncli_stdout 2>&$_ncli_stderr {_ncli_stdout}>&- {_ncli_stderr}>&-
  unset _ncli_stdout _ncli_stderr
  local -i waited
  while [[ -e $_ncli_partial ]] && (( waited++ < 100 )); do
    zselect -t 1
  done
  zf_rm -f -- $_ncli_partial
  unset _ncli_partial
}

autoload -Uz add-zsh-hook
add-zsh-hook preexec _ncli_preexec
add-zsh-hook precmd _ncli_precmd
