typeset -g _ncli_capture=NCLI_CAPTURE
typeset -gaU ncli_uncaptured=(
  ncli vi vim nvim nano emacs less more most man top htop btop watch
  ssh mosh tmux screen fzf claude codex
)

_ncli_preexec() {
  local command=${${(z)1}[1]}
  (( ${ncli_uncaptured[(Ie)${command:t}]} )) && return
  exec {_ncli_stdout}>&1 {_ncli_stderr}>&2
  exec > >(tee -i -- "$_ncli_capture") 2>&1
}

_ncli_precmd() {
  (( ${+_ncli_stdout} )) || return
  exec >&$_ncli_stdout 2>&$_ncli_stderr {_ncli_stdout}>&- {_ncli_stderr}>&-
  unset _ncli_stdout _ncli_stderr
}

autoload -Uz add-zsh-hook
add-zsh-hook preexec _ncli_preexec
add-zsh-hook precmd _ncli_precmd
