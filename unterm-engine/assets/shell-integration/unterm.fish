# Unterm shell integration for fish, found through XDG_DATA_DIRS.
if status is-interactive; and not set -q __UNTERM_FISH_LOADED
    set -g __UNTERM_FISH_LOADED 1
    function __unterm_prompt --on-event fish_prompt
        printf '\e]133;A\a'
    end
    function __unterm_preexec --on-event fish_preexec
        printf '\e]133;C\a'
    end
    function __unterm_postexec --on-event fish_postexec
        printf '\e]133;D;%s\a' $status
    end
    function __unterm_pwd --on-variable PWD
        printf '\e]7;file://%s%s\a' (hostname) $PWD
    end
    __unterm_pwd
end
