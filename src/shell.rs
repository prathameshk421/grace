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
      add|tag|tool|list|pin|unpin|forget|import|init|help|-h|--help|-V|--version)
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

/// Build the zsh script. `names` are command names. Invalid names are skipped,
/// so a bad database row cannot add shell code to the script.
pub fn init_zsh(names: &[String]) -> String {
    let names: Vec<&str> = names
        .iter()
        .map(|n| n.as_str())
        .filter(|n| tools::valid_name(n))
        .collect();
    ZSH_TEMPLATE.replace("@TOOLS@", &names.join("|"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_has_function_and_tools() {
        let r = tools::Registry::new(&[("kiro".into(), "kiro".into(), "kiro".into())]);
        let mut names = r.command_names();
        names.push("x;rm -rf".into());
        let s = init_zsh(&names);
        assert!(s.contains("grace() {"));
        assert!(s.contains("claude|agy|antigravity"));
        assert!(s.contains("cursor-agent"));
        assert!(s.contains("|kiro"));
        assert!(!s.contains("rm -rf"));
        assert!(s.contains("add|tag|tool|"));
        assert!(!s.contains("@TOOLS@"));
    }
}
