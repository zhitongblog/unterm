# Unterm shell integration for zsh: OSC 133 prompt and command marks, and
# OSC 7 for the working directory. Safe to source twice.
[[ -n "${__UNTERM_ZSH_LOADED-}" ]] && return
typeset -g __UNTERM_ZSH_LOADED=1
typeset -g __unterm_running=0

__unterm_precmd() {
  local ret=$?
  if (( __unterm_running )); then
    builtin print -nr -- $'\e]133;D;'"${ret}"$'\a'
  fi
  __unterm_running=0
  builtin print -nr -- $'\e]7;file://'"${HOST}${PWD}"$'\a'
  builtin print -nr -- $'\e]133;A\a'
}

__unterm_preexec() {
  __unterm_running=1
  builtin print -nr -- $'\e]133;C\a'
}

__unterm_line_init() {
  builtin print -nr -- $'\e]133;B\a'
}

autoload -Uz add-zsh-hook
add-zsh-hook precmd __unterm_precmd
add-zsh-hook preexec __unterm_preexec
if autoload -Uz add-zle-hook-widget 2>/dev/null; then
  add-zle-hook-widget line-init __unterm_line_init
fi
