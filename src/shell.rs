//! Shell integration.
use crate::tools;

const ZSH_TEMPLATE: &str = r#"if [[ -z $__GRACE_LOADED ]]; then
  typeset -g __GRACE_LOADED=1
  autoload -Uz add-zsh-hook

  __grace_chpwd() {
    command grace add "$PWD" &>/dev/null &!
  }

  __grace_preexec() {
    local w a tgt i
    local -a ws
    ws=(${(z)1})
    for (( i = 1; i <= $#ws; i++ )); do
      w=${ws[i]}
      case $w in
        [A-Za-z_]*=*|sudo|command|nohup|exec|time|env|noglob) continue ;;
      esac
      w=${(Q)w}
      w=${w:t}
      case $w in
        @TOOLS@)
          tgt=
          for a in "${(@)ws[i+1,-1]}"; do
            a=${(Q)a}
            [[ $a == -* ]] && continue
            tgt=$a
            break
          done
          command grace tag "$PWD" "$w" ${tgt:+"$tgt"} &>/dev/null &!
          case $w in
            @AI@)
              # Live session: start now, end in precmd.
              local sdir=$PWD
              [[ -n $tgt && -d ${tgt/#\~/$HOME} ]] && sdir=${tgt/#\~/$HOME}
              command grace session start "$sdir" "$w" --pid $$ &>/dev/null
              typeset -g __grace_session=1
              ;;
          esac
          ;;
      esac
      break
    done
  }

  __grace_precmd() {
    [[ -n $__grace_session ]] || return
    unset __grace_session
    command grace session end --pid $$ &>/dev/null &!
  }

  add-zsh-hook chpwd __grace_chpwd
  add-zsh-hook preexec __grace_preexec
  add-zsh-hook precmd __grace_precmd

  # Record the folder where the shell starts.
  command grace add "$PWD" &>/dev/null &!

  grace() {
    case $1 in
      add|tag|tool|list|pin|unpin|forget|import|init|setup|session|help|-h|--help|-V|--version)
        command grace "$@"
        return $?
        ;;
    esac
    local sub=pick x
    case $1 in
      sessions)
        shift
        sub=sessions
        for x in "$@"; do
          case $x in
            -l|--list|-h|--help) command grace sessions "$@"; return $? ;;
          esac
        done
        ;;
      pick) shift ;;
    esac
    local out dir cmd
    out="$(command grace $sub "$@")" || return
    dir=${out%%$'\n'*}
    if [[ $out == *$'\n'* ]]; then
      cmd=${out#*$'\n'}
    fi
    # An empty line 1 means: stay here, only run line 2.
    [[ -n $dir || -n $cmd ]] || return 1
    if [[ -n $dir ]]; then
      builtin cd -- "$dir" || return
    fi
    if [[ -n $cmd ]]; then
      eval "$cmd"
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
    local line num rest w a tgt i
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
      for ((i = 0; i < ${#words[@]}; i++)); do
        w=${words[i]}
        case $w in
          [A-Za-z_]*=*|sudo|command|nohup|exec|time|env) continue ;;
        esac
        w=${w##*/}
        case $w in
          @TOOLS@)
            tgt=
            for a in "${words[@]:i+1}"; do
              a=${a#[\"\']}
              a=${a%[\"\']}
              [[ -z $a || $a == -* ]] && continue
              tgt=$a
              break
            done
            (command grace tag "$PWD" "$w" ${tgt:+"$tgt"} &>/dev/null &)
            ;;
        esac
        break
      done
    fi
    return $rc
  }

  # Bash has no reliable preexec, so the shell side of live sessions is skipped here.
  # The agent hooks (grace setup) still report the status.

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
      add|tag|tool|list|pin|unpin|forget|import|init|setup|session|help|-h|--help|-V|--version)
        command grace "$@"
        return $?
        ;;
    esac
    local sub=pick x
    case ${1-} in
      sessions)
        shift
        sub=sessions
        for x in "$@"; do
          case $x in
            -l|--list|-h|--help) command grace sessions "$@"; return $? ;;
          esac
        done
        ;;
      pick) shift ;;
    esac
    local out dir cmd=
    out=$(command grace $sub "$@") || return
    dir=${out%%$'\n'*}
    if [[ $out == *$'\n'* ]]; then
      cmd=${out#*$'\n'}
    fi
    # An empty line 1 means: stay here, only run line 2.
    [[ -n $dir || -n $cmd ]] || return 1
    if [[ -n $dir ]]; then
      builtin cd -- "$dir" || return
    fi
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
        set -l ws (string split -n ' ' -- $argv[1])
        set -l n (count $ws)
        for i in (seq $n)
            set -l w $ws[$i]
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
                    set -l tgt
                    if test $i -lt $n
                        for a in $ws[(math $i + 1)..-1]
                            set a (string trim -c "\"'" -- $a)
                            test -n "$a"; or continue
                            string match -q -- '-*' $a; and continue
                            set tgt $a
                            break
                        end
                    end
                    command grace tag $PWD $base $tgt >/dev/null 2>&1 &
                    disown 2>/dev/null
                    if contains -- $base @AI@
                        # Live session: start now, end in fish_postexec.
                        set -l sdir $PWD
                        if test -n "$tgt"
                            set -l t (string replace -r -- '^~' $HOME $tgt)
                            test -d "$t"; and set sdir $t
                        end
                        command grace session start $sdir $base --pid $fish_pid >/dev/null 2>&1
                        disown 2>/dev/null
                        set -g __grace_session 1
                    end
            end
            break
        end
    end

    function __grace_postexec --on-event fish_postexec
        set -q __grace_session; or return
        set -e __grace_session
        command grace session end --pid $fish_pid >/dev/null 2>&1 &
        disown 2>/dev/null
    end

    # Record the folder where the shell starts.
    command grace add $PWD >/dev/null 2>&1 &
    disown 2>/dev/null

    function grace
        if set -q argv[1]; and contains -- $argv[1] add tag tool list pin unpin forget import init setup session help -h --help -V --version
            command grace $argv
            return $status
        end
        set -l sub pick
        if set -q argv[1]; and test "$argv[1]" = sessions
            set sub sessions
            set -e argv[1]
            if contains -- -l $argv; or contains -- --list $argv; or contains -- -h $argv; or contains -- --help $argv
                command grace sessions $argv
                return $status
            end
        else if set -q argv[1]; and test "$argv[1]" = pick
            set -e argv[1]
        end
        set -l out (command grace $sub $argv)
        or return
        set -l dir $out[1]
        # An empty line 1 means: stay here, only run line 2.
        if test -n "$dir"
            builtin cd -- $dir; or return
        else if not set -q out[2]; or test -z "$out[2]"
            return 1
        end
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
            $ws = @($h.CommandLine.Trim() -split '\s+')
            for ($i = 0; $i -lt $ws.Count; $i++) {
              $w = $ws[$i]
              if ($w -eq '&' -or $w -eq '') { continue }
              $w = $w.Trim('"', "'") -replace '^.*[\\/]', '' -replace '\.(exe|cmd|ps1|bat)$', ''
              if ($global:__GraceTools -contains $w) {
                $tgt = $null
                for ($j = $i + 1; $j -lt $ws.Count; $j++) {
                  $a = $ws[$j].Trim('"', "'")
                  if ($a -eq '' -or $a.StartsWith('-')) { continue }
                  $tgt = $a
                  break
                }
                if ($tgt) { & $exe tag $cwd $w $tgt *> $null } else { & $exe tag $cwd $w *> $null }
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
    $passthrough = @('add', 'tag', 'tool', 'list', 'pin', 'unpin', 'forget', 'import', 'init', 'setup', 'session', 'help', '-h', '--help', '-V', '--version')
    if ($args.Count -gt 0 -and ($passthrough -ccontains [string]$args[0])) {
      & $exe @args
      return
    }
    # No shell side for live sessions here: PowerShell has no reliable preexec.
    # The agent hooks (grace setup) still report the status.
    $rest = @($args)
    $sub = 'pick'
    if ($rest.Count -gt 0 -and $rest[0] -ceq 'sessions') {
      $sub = 'sessions'
      $rest = @($rest | Select-Object -Skip 1)
      if (@($rest | Where-Object { $_ -in '-l', '--list', '-h', '--help' }).Count -gt 0) {
        & $exe sessions @rest
        return
      }
    } elseif ($rest.Count -gt 0 -and $rest[0] -ceq 'pick') {
      $rest = @($rest | Select-Object -Skip 1)
    }
    $out = @(& $exe $sub @rest)
    if ($LASTEXITCODE -ne 0) { return }
    if ($out.Count -eq 0) { return }
    # An empty line 1 means: stay here, only run line 2.
    if ($out[0]) { Set-Location -LiteralPath $out[0] }
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
/// `ai_names` are the AI and custom commands (no editors) that start live sessions.
pub fn init(shell: Shell, names: &[String], ai_names: &[String]) -> String {
    let valid = |v: &[String]| -> Vec<String> {
        v.iter().filter(|n| tools::valid_name(n)).cloned().collect()
    };
    let (names, ai) = (valid(names), valid(ai_names));
    let join = |v: &[String], sep: &str| -> String {
        if v.is_empty() {
            "__grace_none__".to_string()
        } else {
            v.join(sep)
        }
    };
    let (template, joined, ai_joined) = match shell {
        Shell::Zsh => (ZSH_TEMPLATE, join(&names, "|"), join(&ai, "|")),
        Shell::Bash => (BASH_TEMPLATE, join(&names, "|"), join(&ai, "|")),
        Shell::Fish => (FISH_TEMPLATE, join(&names, " "), join(&ai, " ")),
        Shell::PowerShell => (POWERSHELL_TEMPLATE, join(&names, "','"), join(&ai, "','")),
    };
    template.replace("@TOOLS@", &joined).replace("@AI@", &ai_joined)
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

    fn ai_names() -> Vec<String> {
        let r = tools::Registry::new(&[("kiro".into(), "kiro".into(), "kiro".into())]);
        r.ai_command_names()
    }

    fn script(sh: Shell) -> String {
        init(sh, &names(), &ai_names())
    }

    #[test]
    fn zsh_has_function_and_tools() {
        let s = script(Shell::Zsh);
        assert!(s.contains("grace() {"));
        assert!(s.contains("claude|agy|antigravity"));
        assert!(s.contains("cursor-agent"));
        assert!(s.contains("|kiro"));
        assert!(!s.contains("rm -rf"));
        assert!(s.contains("add|tag|tool|"));
        assert!(!s.contains("@TOOLS@"));
        assert!(!s.contains("@AI@"));
    }

    #[test]
    fn sessions_hooks_for_ai_only() {
        let z = script(Shell::Zsh);
        assert!(z.contains("session start"));
        assert!(z.contains("session end"));
        let (_, after) = z.split_once("case $w in\n            ").unwrap();
        let ai_pat = after.lines().next().unwrap();
        assert!(ai_pat.contains("claude|") && ai_pat.contains("kiro"), "{ai_pat}");
        assert!(!ai_pat.contains("zed") && !ai_pat.contains("nvim"), "{ai_pat}");
        let f = script(Shell::Fish);
        assert!(f.contains("session start") && f.contains("fish_postexec"));
        assert!(f.contains("contains -- $base claude"));
        assert!(!f.contains("contains -- $base claude agy antigravity codex gemini cursor cursor-agent opencode aider copilot amp goose crush qwen t3 code"));
    }

    #[test]
    fn sessions_routing_in_every_shell() {
        for sh in [Shell::Zsh, Shell::Bash, Shell::Fish, Shell::PowerShell] {
            let s = script(sh);
            assert!(s.contains("sessions"), "{sh:?}");
            assert!(s.contains("--list"), "{sh:?}");
            assert!(s.contains("setup"), "{sh:?}");
            assert!(s.contains("session"), "{sh:?}");
        }
        assert!(script(Shell::Zsh).contains("sessions)"));
        assert!(script(Shell::Bash).contains("sessions)"));
    }

    #[test]
    fn every_shell_has_function_and_tool() {
        for (sh, func, tool) in [
            (Shell::Bash, "grace() {", "claude|agy"),
            (Shell::Fish, "function grace", "case claude agy"),
            (Shell::PowerShell, "function global:grace", "'claude','agy'"),
        ] {
            let s = script(sh);
            assert!(s.contains(func), "{sh:?}");
            assert!(s.contains(tool), "{sh:?}");
            assert!(s.contains("kiro"), "{sh:?}");
            assert!(!s.contains("rm -rf"), "{sh:?}");
            assert!(!s.contains("@TOOLS@"), "{sh:?}");
            assert!(s.contains("tgt"), "{sh:?}");
        }
    }

    #[test]
    fn parses_shell_names() {
        assert_eq!(Shell::parse("pwsh"), Some(Shell::PowerShell));
        assert_eq!(Shell::parse("bash"), Some(Shell::Bash));
        assert_eq!(Shell::parse("nu"), None);
    }
}
