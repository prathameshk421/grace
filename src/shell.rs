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

const BASH_TEMPLATE: &str = r##"if [[ -z ${__GRACE_LOADED:-} ]]; then
  __GRACE_LOADED=1
  __GRACE_LAST_PWD=$PWD
  __GRACE_LAST_HIST=

  __grace_hook() {
    local rc=$?
    local line num rest w
    local -a words
    if [[ $PWD != "$__GRACE_LAST_PWD" ]]; then
      __GRACE_LAST_PWD=$PWD
      (command grace add "$PWD" &>/dev/null &)
    fi
    line=$(HISTTIMEFORMAT= history 1 2>/dev/null)
    line=${line#"${line%%[![:space:]]*}"}
    num=${line%%[[:space:]]*}
    rest=${line#"$num"}
    rest=${rest#"${rest%%[![:space:]]*}"}
    num=${num%\*}
    if [[ -n $num && $num != "$__GRACE_LAST_HIST" ]]; then
      __GRACE_LAST_HIST=$num
      read -ra words <<<"$rest"
      for w in "${words[@]}"; do
        case $w in
          [A-Za-z_]*=*|sudo|command|nohup|exec|time|env) continue ;;
        esac
        w=${w##*/}
        case $w in
          @TOOLS@)
            (command grace tag "$PWD" "$w" &>/dev/null &)
            ;;
        esac
        break
      done
    fi
    return $rc
  }

  # Do not tag the last command of an older session.
  __GRACE_LAST_HIST=$(HISTTIMEFORMAT= history 1 2>/dev/null)
  __GRACE_LAST_HIST=${__GRACE_LAST_HIST#"${__GRACE_LAST_HIST%%[![:space:]]*}"}
  __GRACE_LAST_HIST=${__GRACE_LAST_HIST%%[[:space:]]*}
  __GRACE_LAST_HIST=${__GRACE_LAST_HIST%\*}

  if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == "declare -a"* ]]; then
    PROMPT_COMMAND=(__grace_hook "${PROMPT_COMMAND[@]}")
  else
    PROMPT_COMMAND="__grace_hook${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
  fi

  # Record the folder where the shell starts.
  (command grace add "$PWD" &>/dev/null &)

  grace() {
    case ${1-} in
      add|tag|tool|list|pin|unpin|forget|import|init|help|-h|--help|-V|--version)
        command grace "$@"
        return $?
        ;;
    esac
    if [[ ${1-} == pick ]]; then
      shift
    fi
    local out dir cmd=
    out=$(command grace pick "$@") || return
    dir=${out%%$'\n'*}
    if [[ $out == *$'\n'* ]]; then
      cmd=${out#*$'\n'}
    fi
    [[ -n $dir ]] || return 1
    builtin cd -- "$dir" || return
    if [[ -n $cmd ]]; then
      eval "$cmd"
    fi
  }
fi
"##;

const FISH_TEMPLATE: &str = r##"if not set -q __GRACE_LOADED
    set -g __GRACE_LOADED 1

    function __grace_pwd --on-variable PWD
        command grace add $PWD >/dev/null 2>&1 &
        disown 2>/dev/null
    end

    function __grace_preexec --on-event fish_preexec
        for w in (string split ' ' -- $argv[1])
            test -n "$w"; or continue
            switch $w
                case sudo command nohup exec time env
                    continue
            end
            if string match -qr '^[A-Za-z_][A-Za-z0-9_]*=' -- $w
                continue
            end
            set -l base (string replace -r '^.*/' '' -- $w)
            switch $base
                case @TOOLS@
                    command grace tag $PWD $base >/dev/null 2>&1 &
                    disown 2>/dev/null
            end
            break
        end
    end

    # Record the folder where the shell starts.
    command grace add $PWD >/dev/null 2>&1 &
    disown 2>/dev/null

    function grace
        if set -q argv[1]; and contains -- $argv[1] add tag tool list pin unpin forget import init help -h --help -V --version
            command grace $argv
            return $status
        end
        if set -q argv[1]; and test "$argv[1]" = pick
            set -e argv[1]
        end
        set -l out (command grace pick $argv)
        or return
        set -l dir $out[1]
        test -n "$dir"; or return 1
        builtin cd -- $dir; or return
        if set -q out[2]; and test -n "$out[2]"
            eval $out[2]
        end
    end
end
"##;

const POWERSHELL_TEMPLATE: &str = r##"if (-not $global:__GraceLoaded) {
  $global:__GraceLoaded = $true
  $global:__GraceExe = (Get-Command grace -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1).Source
  $global:__GraceTools = @('@TOOLS@')
  $global:__GraceLastPwd = $PWD.Path
  $global:__GraceLastId = 0
  $__graceHist = Get-History -Count 1 -ErrorAction SilentlyContinue
  if ($__graceHist) { $global:__GraceLastId = $__graceHist.Id }
  Remove-Variable __graceHist
  $global:__GraceOldPrompt = $function:prompt

  # Record the folder where the shell starts.
  if ($global:__GraceExe) {
    try { & $global:__GraceExe add $PWD.Path *> $null } catch { }
  }

  function global:prompt {
    $savedExit = $global:LASTEXITCODE
    $exe = $global:__GraceExe
    if ($exe) {
      try {
        if ($PWD.Provider.Name -eq 'FileSystem') {
          $cwd = $PWD.Path
          if ($cwd -ne $global:__GraceLastPwd) {
            $global:__GraceLastPwd = $cwd
            & $exe add $cwd *> $null
          }
          $h = Get-History -Count 1 -ErrorAction SilentlyContinue
          if ($h -and $h.Id -ne $global:__GraceLastId) {
            $global:__GraceLastId = $h.Id
            foreach ($w in ($h.CommandLine.Trim() -split '\s+')) {
              if ($w -eq '&' -or $w -eq '') { continue }
              $w = $w.Trim('"', "'") -replace '^.*[\\/]', '' -replace '\.(exe|cmd|ps1|bat)$', ''
              if ($global:__GraceTools -contains $w) {
                & $exe tag $cwd $w *> $null
              }
              break
            }
          }
        }
      } catch { }
    }
    $global:LASTEXITCODE = $savedExit
    if ($global:__GraceOldPrompt) {
      & $global:__GraceOldPrompt
    } else {
      "PS $($PWD.Path)> "
    }
  }

  function global:grace {
    $exe = $global:__GraceExe
    if (-not $exe) {
      Write-Error 'grace: the grace program is not on PATH'
      return
    }
    $passthrough = @('add', 'tag', 'tool', 'list', 'pin', 'unpin', 'forget', 'import', 'init', 'help', '-h', '--help', '-V', '--version')
    if ($args.Count -gt 0 -and ($passthrough -ccontains [string]$args[0])) {
      & $exe @args
      return
    }
    $rest = @($args)
    if ($rest.Count -gt 0 -and $rest[0] -ceq 'pick') {
      $rest = @($rest | Select-Object -Skip 1)
    }
    $out = @(& $exe pick @rest)
    if ($LASTEXITCODE -ne 0) { return }
    if ($out.Count -eq 0 -or -not $out[0]) { return }
    Set-Location -LiteralPath $out[0]
    if ($out.Count -gt 1 -and $out[1]) { Invoke-Expression $out[1] }
  }
}
"##;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
    PowerShell,
}

impl Shell {
    pub fn parse(s: &str) -> Option<Shell> {
        match s.to_ascii_lowercase().as_str() {
            "zsh" => Some(Shell::Zsh),
            "bash" => Some(Shell::Bash),
            "fish" => Some(Shell::Fish),
            "powershell" | "pwsh" => Some(Shell::PowerShell),
            _ => None,
        }
    }
}

/// Build the script for `shell`. `names` are command names. Invalid names are
/// skipped, so a bad database row cannot add shell code to the script.
pub fn init(shell: Shell, names: &[String]) -> String {
    let names: Vec<&str> = names
        .iter()
        .map(|n| n.as_str())
        .filter(|n| tools::valid_name(n))
        .collect();
    let (template, joined) = match shell {
        Shell::Zsh => (ZSH_TEMPLATE, names.join("|")),
        Shell::Bash => (BASH_TEMPLATE, names.join("|")),
        Shell::Fish => (FISH_TEMPLATE, names.join(" ")),
        Shell::PowerShell => (POWERSHELL_TEMPLATE, names.join("','")),
    };
    template.replace("@TOOLS@", &joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        let r = tools::Registry::new(&[("kiro".into(), "kiro".into(), "kiro".into())]);
        let mut names = r.command_names();
        names.push("x;rm -rf".into());
        names
    }

    #[test]
    fn zsh_has_function_and_tools() {
        let s = init(Shell::Zsh, &names());
        assert!(s.contains("grace() {"));
        assert!(s.contains("claude|agy|antigravity"));
        assert!(s.contains("cursor-agent"));
        assert!(s.contains("|kiro"));
        assert!(!s.contains("rm -rf"));
        assert!(s.contains("add|tag|tool|"));
        assert!(!s.contains("@TOOLS@"));
    }

    #[test]
    fn every_shell_has_function_and_tool() {
        for (sh, func, tool) in [
            (Shell::Bash, "grace() {", "claude|agy"),
            (Shell::Fish, "function grace", "case claude agy"),
            (Shell::PowerShell, "function global:grace", "'claude','agy'"),
        ] {
            let s = init(sh, &names());
            assert!(s.contains(func), "{sh:?}");
            assert!(s.contains(tool), "{sh:?}");
            assert!(s.contains("kiro"), "{sh:?}");
            assert!(!s.contains("rm -rf"), "{sh:?}");
            assert!(!s.contains("@TOOLS@"), "{sh:?}");
        }
    }

    #[test]
    fn parses_shell_names() {
        assert_eq!(Shell::parse("pwsh"), Some(Shell::PowerShell));
        assert_eq!(Shell::parse("bash"), Some(Shell::Bash));
        assert_eq!(Shell::parse("nu"), None);
    }
}
