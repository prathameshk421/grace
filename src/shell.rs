//! Shell integration.
use crate::tools;

const ZSH_TEMPLATE: &str = r#"if [[ -z $__GRACE_LOADED ]]; then
  typeset -g __GRACE_LOADED=1
  autoload -Uz add-zsh-hook

  __grace_chpwd() {
    command grace add "$PWD" &>/dev/null &!
  }

  __grace_preexec() {
    local w
    for w in ${(z)1}; do
      case $w in
        [A-Za-z_]*=*|sudo|command|nohup|exec|time|env|noglob) continue ;;
      esac
      w=${w:t}
      case $w in
        @TOOLS@)
          command grace tag "$PWD" "$w" &>/dev/null &!
          ;;
      esac
      break
    done
  }

  add-zsh-hook chpwd __grace_chpwd
  add-zsh-hook preexec __grace_preexec

  # Record the folder where the shell starts.
  command grace add "$PWD" &>/dev/null &!

  grace() {
    case $1 in
      add|tag|list|pin|unpin|forget|import|init|help|-h|--help|-V|--version)
        command grace "$@"
        return $?
        ;;
    esac
    [[ $1 == pick ]] && shift
    local out dir cmd
    out="$(command grace pick "$@")" || return
    dir=${out%%$'\n'*}
    if [[ $out == *$'\n'* ]]; then
      cmd=${out#*$'\n'}
    fi
    [[ -n $dir ]] || return 1
    builtin cd -- "$dir" || return
    if [[ -n $cmd ]]; then
      ${=cmd}
    fi
  }
fi
"#;

pub fn init_zsh() -> String {
    let names = tools::command_names().join("|");
    ZSH_TEMPLATE.replace("@TOOLS@", &names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_has_function_and_tools() {
        let s = init_zsh();
        assert!(s.contains("grace() {"));
        assert!(s.contains("claude|agy|antigravity"));
        assert!(s.contains("cursor-agent"));
        assert!(!s.contains("@TOOLS@"));
    }
}
