# Unterm shell integration for bash, loaded with --rcfile.
#
# --rcfile replaces the files bash would read, so read them here first:
# the login files when Unterm started bash as a login shell, .bashrc
# otherwise. Then add OSC 133 marks and OSC 7, without a DEBUG trap, so a
# user's own trap or bash-preexec is left alone.
if [[ -n "${UNTERM_BASH_LOGIN-}" ]]; then
  unset UNTERM_BASH_LOGIN
  [[ -f /etc/profile ]] && builtin source /etc/profile
  for __unterm_file in ~/.bash_profile ~/.bash_login ~/.profile; do
    if [[ -f "$__unterm_file" ]]; then
      builtin source "$__unterm_file"
      break
    fi
  done
  unset __unterm_file
else
  [[ -f ~/.bashrc ]] && builtin source ~/.bashrc
fi

if [[ -z "${__UNTERM_BASH_LOADED-}" ]]; then
  __UNTERM_BASH_LOADED=1
  __unterm_first_prompt=1
  __unterm_prompt() {
    local ret=$?
    if [[ -z "$__unterm_first_prompt" ]]; then
      builtin printf '\e]133;D;%s\a' "$ret"
    fi
    __unterm_first_prompt=
    builtin printf '\e]7;file://%s%s\a' "${HOSTNAME}" "${PWD}"
    builtin printf '\e]133;A\a'
    return $ret
  }
  PROMPT_COMMAND="__unterm_prompt${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
  PS1="${PS1}\[\e]133;B\a\]"
  # bash 4.4 and later print PS0 after a command is read, before it runs.
  PS0="${PS0}\[\e]133;C\a\]"
fi
