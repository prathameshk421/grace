//! Shell integration and completion scripts.
use crate::tools;

const ZSH_TEMPLATE: &str = r#"if [[ -z ${__GRACE_LOADED:-} ]]; then
  typeset -g __GRACE_LOADED=1
  typeset -gx GRACE_SHELL=zsh
  autoload -Uz add-zsh-hook

  __grace_chpwd() {
    command grace add -- "$PWD" &>/dev/null &!
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
            [[ -z $a ]] && continue
            [[ $a == (';'|'&&'|'||'|'|'|'&') ]] && break
            [[ $a == -* ]] && continue
            tgt=$a
            break
          done
          case $w in
            @AI@)
              # Coalesce tag + live session start into one call; end in precmd.
              local tdir=${tgt:-}
              if [[ $tdir == '~' ]]; then
                tdir=$HOME
              elif [[ $tdir == '~/'* ]]; then
                tdir="$HOME/${tdir#\~/}"
              fi
              local -a targs=()
              [[ -n $tdir ]] && targs=(--target "$tdir")
              command grace session start --tag --pid $$ "${targs[@]}" -- "$PWD" "$w" &>/dev/null
              typeset -g __grace_session=1
              ;;
            *)
              command grace tag -- "$PWD" "$w" ${tgt:+"$tgt"} &>/dev/null &!
              ;;
          esac
          ;;
      esac
      break
    done
  }

  __grace_precmd() {
    [[ -n ${__grace_session:-} ]] || return 0
    unset __grace_session
    command grace session end --pid $$ &>/dev/null &!
  }

  add-zsh-hook chpwd __grace_chpwd
  add-zsh-hook preexec __grace_preexec
  add-zsh-hook precmd __grace_precmd

  # Record the folder where the shell starts.
  command grace add -- "$PWD" &>/dev/null &!

  grace() {
    local check=${1-}
    local full_opt=
    if [[ $check == --full ]]; then
      check=${2-}
      full_opt=--full
      shift
    fi
    case $check in
      add|tag|tool|list|pin|unpin|forget|import|init|setup|doctor|prune|stats|completions|session|hub|diff|tree|help|-h|--help|-V|--version)
        command grace ${full_opt:+"$full_opt"} "$@"
        return $?
        ;;
    esac
    local sub=pick x
    case ${1-} in
      sessions)
        shift
        sub=sessions
        for x in "$@"; do
          [[ $x == -- ]] && break
          case $x in
            --list|--json|--help|--version|-[^-]*[lhV]*)
              command grace ${full_opt:+"$full_opt"} sessions "$@"
              return $?
              ;;
          esac
        done
        ;;
      pick)
        shift
        for x in "$@"; do
          [[ $x == -- ]] && break
          case $x in
            --help|--version|-[^-]*[hV]*)
              command grace ${full_opt:+"$full_opt"} pick "$@"
              return $?
              ;;
          esac
        done
        ;;
      *)
        for x in "$@"; do
          [[ $x == -- ]] && break
          case $x in
            --help|--version|-[^-]*[hV]*)
              command grace ${full_opt:+"$full_opt"} "$@"
              return $?
              ;;
          esac
        done
        ;;
    esac
    local out dir= cmd=
    out="$(command grace ${full_opt:+"$full_opt"} $sub "$@")" || return $?
    dir=${out%%$'\n'*}
    if [[ $out == *$'\n'* ]]; then
      cmd=${out#*$'\n'}
    fi
    # Empty output with exit 0 means handled directly (e.g. GUI editor launched).
    [[ -n ${dir:-} || -n ${cmd:-} ]] || return 0
    if [[ -n ${dir:-} ]]; then
      builtin cd -- "$dir" || return $?
    fi
    if [[ -n ${cmd:-} ]]; then
      eval "$cmd"
    fi
  }
fi
"#;

const BASH_TEMPLATE: &str = r##"if [[ -z ${__GRACE_LOADED:-} ]]; then
  __GRACE_LOADED=1
  export GRACE_SHELL=bash
  __GRACE_LAST_PWD=$PWD
  __GRACE_LAST_HIST=

  __grace_hook() {
    local rc=$?
    local line num rest w a tgt i
    local -a words
    if [[ $PWD != "${__GRACE_LAST_PWD:-}" ]]; then
      __GRACE_LAST_PWD=$PWD
      (command grace add -- "$PWD" &>/dev/null &)
    fi
    line=$(HISTTIMEFORMAT= history 1 2>/dev/null || true)
    line=${line#"${line%%[![:space:]]*}"}
    num=${line%%[[:space:]]*}
    rest=${line#"$num"}
    rest=${rest#"${rest%%[![:space:]]*}"}
    num=${num%\*}
    if [[ -n $num && $num != "${__GRACE_LAST_HIST:-}" ]]; then
      __GRACE_LAST_HIST=$num
      local re=$'^[[:space:]]*((\\\\.|"(\\\\.|[^"])*"|\'[^\']*\'|[^[:space:]"\'\\\\&|;])+|[&|;]{1,2})'
      local tmp=$rest tok
      words=()
      while [[ $tmp =~ $re ]]; do
        tok=${BASH_REMATCH[1]}
        [[ $tok == ';'* || $tok == '&'* || $tok == '|'* ]] && break
        words+=("$tok")
        tmp=${tmp:${#BASH_REMATCH[0]}}
      done
      for ((i = 0; i < ${#words[@]}; i++)); do
        w=${words[i]}
        if [[ $w == [A-Za-z_]*=* ]]; then
          continue
        fi
        if [[ ${#w} -ge 2 && ( ( $w == \"*\" ) || ( $w == \'*\' ) ) ]]; then
          w=${w:1:${#w}-2}
        fi
        case $w in
          sudo|command|nohup|exec|time|env) continue ;;
        esac
        w=${w##*/}
        case $w in
          @TOOLS@)
            tgt=
            for a in "${words[@]:i+1}"; do
              [[ $a == ';' || $a == ';;' || $a == '&' || $a == '&&' || $a == '|' || $a == '||' || $a == '|&' ]] && break
              if [[ ${#a} -ge 2 && ( ( $a == \"*\" ) || ( $a == \'*\' ) ) ]]; then
                a=${a:1:${#a}-2}
              fi
              [[ -z $a ]] && continue
              [[ $a == -* ]] && continue
              tgt=$a
              break
            done
            (command grace tag -- "$PWD" "$w" ${tgt:+"$tgt"} &>/dev/null &)
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
  __GRACE_LAST_HIST=$(HISTTIMEFORMAT= history 1 2>/dev/null || true)
  __GRACE_LAST_HIST=${__GRACE_LAST_HIST#"${__GRACE_LAST_HIST%%[![:space:]]*}"}
  __GRACE_LAST_HIST=${__GRACE_LAST_HIST%%[[:space:]]*}
  __GRACE_LAST_HIST=${__GRACE_LAST_HIST%\*}

  if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == "declare -a"* ]]; then
    PROMPT_COMMAND=(__grace_hook ${PROMPT_COMMAND[@]+"${PROMPT_COMMAND[@]}"})
  else
    __grace_pc=${PROMPT_COMMAND:-}
    __grace_pc=${__grace_pc#;}
    __grace_pc=${__grace_pc#"${__grace_pc%%[![:space:]]*}"}
    PROMPT_COMMAND="__grace_hook${__grace_pc:+;$__grace_pc}"
    unset __grace_pc
  fi

  # Record the folder where the shell starts.
  (command grace add -- "$PWD" &>/dev/null &)

  grace() {
    local check=${1-}
    local full_opt=
    if [[ $check == --full ]]; then
      check=${2-}
      full_opt=--full
      shift
    fi
    case $check in
      add|tag|tool|list|pin|unpin|forget|import|init|setup|doctor|prune|stats|completions|session|hub|diff|tree|help|-h|--help|-V|--version)
        command grace ${full_opt:+"$full_opt"} "$@"
        return $?
        ;;
    esac
    local sub=pick x
    case ${1-} in
      sessions)
        shift
        sub=sessions
        for x in "$@"; do
          [[ $x == -- ]] && break
          case $x in
            --list|--json|--help|--version|-[^-]*[lhV]*)
              command grace ${full_opt:+"$full_opt"} sessions "$@"
              return $?
              ;;
          esac
        done
        ;;
      pick)
        shift
        for x in "$@"; do
          [[ $x == -- ]] && break
          case $x in
            --help|--version|-[^-]*[hV]*)
              command grace ${full_opt:+"$full_opt"} pick "$@"
              return $?
              ;;
          esac
        done
        ;;
      *)
        for x in "$@"; do
          [[ $x == -- ]] && break
          case $x in
            --help|--version|-[^-]*[hV]*)
              command grace ${full_opt:+"$full_opt"} "$@"
              return $?
              ;;
          esac
        done
        ;;
    esac
    local out dir= cmd=
    out=$(command grace ${full_opt:+"$full_opt"} $sub "$@") || return $?
    dir=${out%%$'\n'*}
    if [[ $out == *$'\n'* ]]; then
      cmd=${out#*$'\n'}
    fi
    # Empty output with exit 0 means handled directly (e.g. GUI editor launched).
    [[ -n ${dir:-} || -n ${cmd:-} ]] || return 0
    if [[ -n ${dir:-} ]]; then
      builtin cd -- "$dir" || return $?
    fi
    if [[ -n ${cmd:-} ]]; then
      eval "$cmd"
    fi
  }
fi
"##;

const FISH_TEMPLATE: &str = r##"if not set -q __GRACE_LOADED
    set -g __GRACE_LOADED 1
    set -gx GRACE_SHELL fish

    function __grace_pwd --on-variable PWD
        command grace add -- $PWD >/dev/null 2>&1 &
        disown 2>/dev/null
    end

    function __grace_preexec --on-event fish_preexec
        set -l ws (string match -ra -- '(?:\\\\.|"(?:\\\\.|[^"])*"|\'[^\']*\'|[^\s"\'\\\\&|;])+|[&|;]{1,2}' -- $argv[1])
        set -l n (count $ws)
        test $n -gt 0; or return
        for i in (seq $n)
            set -l w $ws[$i]
            if contains -- $w ';' ';;' '&' '&&' '|' '||' '|&'
                break
            end
            if string match -qr '^[A-Za-z_][A-Za-z0-9_]*=' -- $w
                continue
            end
            set w (string replace -r -- '^"(.*)"$' '$1' $w)
            set w (string replace -r -- "^\x27(.*)\x27\$" '$1' $w)
            switch $w
                case sudo command nohup exec time env
                    continue
            end
            set -l base (string replace -r '^.*/' '' -- $w)
            switch $base
                case @TOOLS@
                    set -l tgt
                    if test $i -lt $n
                        for a in $ws[(math $i + 1)..-1]
                            contains -- $a ';' ';;' '&' '&&' '|' '||' '|&'; and break
                            set a (string replace -r -- '^"(.*)"$' '$1' $a)
                            set a (string replace -r -- "^\x27(.*)\x27\$" '$1' $a)
                            test -n "$a"; or continue
                            string match -q -- '-*' $a; and continue
                            set tgt $a
                            break
                        end
                    end
                    if contains -- $base @AI@
                        # Coalesce tag + live session start into one call; end in fish_postexec.
                        set -l tdir $tgt
                        if test "$tdir" = '~'
                            set tdir $HOME
                        else if string match -q -- '~/*' $tdir
                            set tdir "$HOME/"(string sub -s 3 -- $tdir)
                        end
                        if test -n "$tdir"
                            command grace session start --tag --pid $fish_pid --target "$tdir" -- $PWD $base >/dev/null 2>&1
                        else
                            command grace session start --tag --pid $fish_pid -- $PWD $base >/dev/null 2>&1
                        end
                        set -g __grace_session 1
                    else
                        command grace tag -- $PWD $base $tgt >/dev/null 2>&1 &
                        disown 2>/dev/null
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
    command grace add -- $PWD >/dev/null 2>&1 &
    disown 2>/dev/null

    function grace
        set -l extra
        set -l check ""
        if set -q argv[1]; and test "$argv[1]" = --full
            set extra --full
            set -e argv[1]
        end
        if set -q argv[1]
            set check $argv[1]
        end
        if test -n "$check"; and contains -- $check add tag tool list pin unpin forget import init setup doctor prune stats completions session hub diff tree help -h --help -V --version
            command grace $extra $argv
            return $status
        end
        set -l sub pick
        if set -q argv[1]; and test "$argv[1]" = sessions
            set sub sessions
            set -e argv[1]
            for x in $argv
                test "$x" = --; and break
                if contains -- $x --list --json --help --version; or string match -qr '^-[^-]*[lhV]' -- $x
                    command grace $extra sessions $argv
                    return $status
                end
            end
        else if set -q argv[1]; and test "$argv[1]" = pick
            set -e argv[1]
            for x in $argv
                test "$x" = --; and break
                if contains -- $x --help --version; or string match -qr '^-[^-]*[hV]' -- $x
                    command grace $extra pick $argv
                    return $status
                end
            end
        else
            for x in $argv
                test "$x" = --; and break
                if contains -- $x --help --version; or string match -qr '^-[^-]*[hV]' -- $x
                    command grace $extra $argv
                    return $status
                end
            end
        end
        set -l out (command grace $extra $sub $argv)
        or return $status
        if test (count $out) -eq 0
            return 0
        end
        set -l dir $out[1]
        # An empty line 1 means: stay here, only run line 2.
        if test -n "$dir"
            builtin cd -- $dir; or return $status
        else if not set -q out[2]; or test -z "$out[2]"
            return 0
        end
        if set -q out[2]; and test -n "$out[2]"
            eval $out[2]
        end
    end
end
"##;

const POWERSHELL_TEMPLATE: &str = r##"if (-not $global:__GraceLoaded) {
  $global:__GraceLoaded = $true
  $env:GRACE_SHELL = 'powershell'
  $global:__GraceExe = $null
  $global:__GraceTools = @('@TOOLS@')
  $global:__GraceLastPwd = $PWD.Path
  $global:__GraceLastId = 0
  $__graceHist = Get-History -Count 1 -ErrorAction SilentlyContinue
  if ($__graceHist) { $global:__GraceLastId = $__graceHist.Id }
  Remove-Variable __graceHist
  $global:__GraceOldPrompt = $function:prompt

  function global:__GraceResolveExe {
    if ($global:__GraceExe -and [System.IO.File]::Exists($global:__GraceExe)) {
      return $global:__GraceExe
    }
    $cmd = Get-Command grace -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($cmd -and $cmd.Source) {
      $global:__GraceExe = $cmd.Source
    } else {
      $global:__GraceExe = $null
    }
    return $global:__GraceExe
  }

  function global:__GraceSpawn {
    param([string]$Exe, [string[]]$ArgList)
    $escaped = foreach ($arg in $ArgList) {
      if ($null -eq $arg) { continue }
      $s = [string]$arg
      if ($s -eq '' -or $s -match '[\s"]') {
        '"' + ($s -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"'
      } else {
        $s
      }
    }
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Exe
    $psi.Arguments = ($escaped -join ' ')
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $p = [System.Diagnostics.Process]::Start($psi)
    if ($p) { $p.Dispose() }
  }

  # Record the folder where the shell starts.
  $__graceInitExe = __GraceResolveExe
  if ($__graceInitExe) {
    try { __GraceSpawn -Exe $__graceInitExe -ArgList @('add', '--', $PWD.Path) } catch { }
  }
  Remove-Variable __graceInitExe -ErrorAction SilentlyContinue

  function global:prompt {
    $savedExit = $global:LASTEXITCODE
    # Invoke previous prompt first so it observes the caller's real $? and $LASTEXITCODE.
    $promptOut = if ($global:__GraceOldPrompt) {
      & $global:__GraceOldPrompt
    } else {
      "PS $($PWD.Path)> "
    }
    $exe = __GraceResolveExe
    if ($exe) {
      $oldEap = $ErrorActionPreference
      $ErrorActionPreference = 'Continue'
      try {
        if ($PWD.Provider.Name -eq 'FileSystem') {
          $cwd = $PWD.Path
          if ($cwd -ne $global:__GraceLastPwd) {
            $global:__GraceLastPwd = $cwd
            __GraceSpawn -Exe $exe -ArgList @('add', '--', $cwd)
          }
          $h = Get-History -Count 1 -ErrorAction SilentlyContinue
          if ($h -and $h.Id -ne $global:__GraceLastId -and $h.CommandLine) {
            $global:__GraceLastId = $h.Id
            $ws = @([regex]::Matches($h.CommandLine.Trim(), '(?:[^\s"'';&|]+|"[^"]*"|''[^'']*'')+|&&|\|\||[;|&]') | ForEach-Object { $_.Value })
            for ($i = 0; $i -lt $ws.Count; $i++) {
              $rawW = $ws[$i]
              if ($rawW -eq '&' -and $i -eq 0) { continue }
              if ($rawW -in ';', '&', '&&', '|', '||') { break }
              $w = ($rawW -replace '^"(.*)"$', '$1' -replace "^'(.*)'$", '$1') -replace '^.*[\\/]', '' -replace '\.(exe|cmd|ps1|bat)$', ''
              if ($w -eq '') { continue }
              if ($global:__GraceTools -contains $w) {
                $tgt = $null
                for ($j = $i + 1; $j -lt $ws.Count; $j++) {
                  $rawA = $ws[$j]
                  if ($rawA -in ';', '&', '&&', '|', '||') { break }
                  $a = $rawA -replace '^"(.*)"$', '$1' -replace "^'(.*)'$", '$1'
                  if ($a -eq '' -or $a.StartsWith('-')) { continue }
                  $tgt = $a
                  break
                }
                if ($tgt) {
                  __GraceSpawn -Exe $exe -ArgList @('tag', '--', $cwd, $w, $tgt)
                } else {
                  __GraceSpawn -Exe $exe -ArgList @('tag', '--', $cwd, $w)
                }
              }
              break
            }
          }
        }
      } catch { }
      finally {
        $ErrorActionPreference = $oldEap
      }
    }
    $global:LASTEXITCODE = $savedExit
    return $promptOut
  }

  function global:grace {
    $exe = __GraceResolveExe
    if (-not $exe) {
      Write-Error 'grace: the grace program is not on PATH'
      return
    }
    $passthrough = @('add', 'tag', 'tool', 'list', 'pin', 'unpin', 'forget', 'import', 'init', 'setup', 'doctor', 'prune', 'stats', 'completions', 'session', 'hub', 'diff', 'tree', 'help', '-h', '--help', '-V', '--version')
    $rest = @($args)
    $extra = @()
    if ($rest.Count -gt 0 -and ([string]$rest[0] -ceq '--full')) {
      $extra = @('--full')
      $rest = @($rest | Select-Object -Skip 1)
    }
    if ($rest.Count -gt 0 -and ($passthrough -ccontains [string]$rest[0])) {
      & $exe @extra @rest
      return
    }
    # No shell side for live sessions here: PowerShell has no reliable preexec.
    # The agent hooks (grace setup) still report the status.
    $sub = 'pick'
    if ($rest.Count -gt 0 -and ([string]$rest[0] -ceq 'sessions')) {
      $sub = 'sessions'
      $rest = @($rest | Select-Object -Skip 1)
      foreach ($x in $rest) {
        $xs = [string]$x
        if ($xs -ceq '--') { break }
        if ($xs -cin @('--list', '--json', '--help', '--version') -or ($xs -cmatch '^-[^-]*[lhV]')) {
          & $exe @extra sessions @rest
          return
        }
      }
    } elseif ($rest.Count -gt 0 -and ([string]$rest[0] -ceq 'pick')) {
      $rest = @($rest | Select-Object -Skip 1)
      foreach ($x in $rest) {
        $xs = [string]$x
        if ($xs -ceq '--') { break }
        if ($xs -cin @('--help', '--version') -or ($xs -cmatch '^-[^-]*[hV]')) {
          & $exe @extra pick @rest
          return
        }
      }
    } else {
      foreach ($x in $rest) {
        $xs = [string]$x
        if ($xs -ceq '--') { break }
        if ($xs -cin @('--help', '--version') -or ($xs -cmatch '^-[^-]*[hV]')) {
          & $exe @extra @rest
          return
        }
      }
    }
    $out = @(& $exe @extra $sub @rest)
    if ($LASTEXITCODE -ne 0) { return }
    if ($out.Count -eq 0) { return }
    # An empty line 1 means: stay here, only run line 2.
    if ($out[0]) {
      try {
        Set-Location -LiteralPath $out[0] -ErrorAction Stop
      } catch {
        return
      }
    }
    if ($out.Count -gt 1 -and $out[1]) { Invoke-Expression $out[1] }
  }
}
"##;

const ZSH_COMPLETIONS: &str = r#"#compdef grace
_grace() {
  local -a cmds
  cmds=(
    'pick:Open the picker and print the chosen path'
    'list:Print tracked folders, best first'
    'pin:Keep a folder at the top'
    'unpin:Remove the pin from a folder'
    'forget:Remove a folder from grace'
    'import:Import folders from Claude Code and editors'
    'tool:Add, remove or list AI tools'
    'sessions:Show folders with active CLI agent sessions'
    'setup:Connect CLI agents for live session status'
    'doctor:Check environment, hooks, database, and tmux'
    'prune:Remove tracked folders missing on disk'
    'stats:Show usage statistics'
    'hub:Open tmux multi-pane workspace hub'
    'diff:Show git changes for a folder'
    'tree:Show file tree with git status'
    'init:Print shell setup code'
    'completions:Print shell completion script'
  )
  _arguments -C \
    '--full[Draw picker fullscreen]' \
    '-t[Filter by tool tag]:tag:' '--tag[Filter by tool tag]:tag:' \
    '-o[Launch last AI tool]' '--open[Launch last AI tool]' \
    '-h[Print help]' '--help[Print help]' \
    '-V[Print version]' '--version[Print version]' \
    '1:command:->cmd' \
    '*::arg:->args'
  case $state in
    cmd) _describe -t commands 'grace command' cmds ;;
    args)
      case $words[1] in
        init|completions) compadd zsh bash fish powershell pwsh ;;
        setup) compadd -- --all --remove claude codex gemini qwen copilot opencode agy crush goose amp aider ;;
        import) compadd all claude vscode cursor windsurf antigravity zed t3code ;;
        doctor) compadd -- --fix ;;
        prune) compadd -- --dry-run --older-than ;;
        list) compadd -- -t --tag --json ;;
        pick) compadd -- -t --tag -o --open ;;
        sessions) compadd -- -l --list --json -o --open ;;
        pin|unpin|forget|diff|tree) _files -/ ;;
        hub) compadd quit; _files -/ ;;
        tool)
          if (( CURRENT >= 3 )) && [[ $words[2] == add ]]; then
            compadd -- --tag --launch
          else
            compadd add rm list
          fi
          ;;
      esac
      ;;
  esac
}
compdef _grace grace
"#;

const BASH_COMPLETIONS: &str = r#"_grace_completions() {
  local cur prev cmds sub="" sub_idx=0 i
  COMPREPLY=()
  cur="${COMP_WORDS[COMP_CWORD]}"
  prev="${COMP_WORDS[COMP_CWORD-1]}"
  cmds="pick list pin unpin forget import tool sessions setup doctor prune stats hub diff tree init completions"
  for ((i = 1; i < COMP_CWORD; i++)); do
    case "${COMP_WORDS[i]}" in
      -t|--tag) ((i++)); continue ;;
      --) break ;;
      --full|-o|--open) continue ;;
      -*) continue ;;
      *) sub="${COMP_WORDS[i]}"; sub_idx=$i; break ;;
    esac
  done
  if [[ -z "$sub" ]]; then
    if [[ "$prev" == -t || "$prev" == --tag ]]; then
      return 0
    fi
    COMPREPLY=( $(compgen -W "$cmds -t --tag -o --open --full -h --help -V --version" -- "$cur") )
    return 0
  fi
  case "$sub" in
    init|completions)
      COMPREPLY=( $(compgen -W "zsh bash fish powershell pwsh" -- "$cur") ) ;;
    setup)
      COMPREPLY=( $(compgen -W "--all --remove claude codex gemini qwen copilot opencode agy crush goose amp aider" -- "$cur") ) ;;
    import)
      COMPREPLY=( $(compgen -W "all claude vscode cursor windsurf antigravity zed t3code" -- "$cur") ) ;;
    doctor)
      COMPREPLY=( $(compgen -W "--fix" -- "$cur") ) ;;
    prune)
      COMPREPLY=( $(compgen -W "--dry-run --older-than" -- "$cur") ) ;;
    list)
      COMPREPLY=( $(compgen -W "-t --tag --json" -- "$cur") ) ;;
    pick)
      COMPREPLY=( $(compgen -W "-t --tag -o --open" -- "$cur") ) ;;
    sessions)
      COMPREPLY=( $(compgen -W "-l --list --json -o --open" -- "$cur") ) ;;
    pin|unpin|forget|diff|tree)
      COMPREPLY=( $(compgen -d -- "$cur") ) ;;
    hub)
      COMPREPLY=( $(compgen -W "quit" -- "$cur") $(compgen -d -- "$cur") ) ;;
    tool)
      if (( sub_idx + 1 < COMP_CWORD )) && [[ "${COMP_WORDS[sub_idx+1]}" == add ]]; then
        COMPREPLY=( $(compgen -W "--tag --launch" -- "$cur") )
      else
        COMPREPLY=( $(compgen -W "add rm list" -- "$cur") )
      fi
      ;;
  esac
}
complete -F _grace_completions grace
"#;

const FISH_COMPLETIONS: &str = r#"complete -c grace -f
complete -c grace -l full -d "Draw picker fullscreen"
complete -c grace -s h -l help -d "Print help"
complete -c grace -s V -l version -d "Print version"
complete -c grace -n "__fish_use_subcommand" -s t -l tag -x -d "Filter by tool tag"
complete -c grace -n "__fish_use_subcommand" -s o -l open -d "Launch last AI tool"
complete -c grace -n "__fish_use_subcommand" -a "pick" -d "Open the folder picker"
complete -c grace -n "__fish_use_subcommand" -a "list" -d "Print tracked folders"
complete -c grace -n "__fish_use_subcommand" -a "pin" -d "Pin folder"
complete -c grace -n "__fish_use_subcommand" -a "unpin" -d "Unpin folder"
complete -c grace -n "__fish_use_subcommand" -a "forget" -d "Forget folder"
complete -c grace -n "__fish_use_subcommand" -a "import" -d "Import from editors/Claude"
complete -c grace -n "__fish_use_subcommand" -a "tool" -d "Manage tracked tools"
complete -c grace -n "__fish_use_subcommand" -a "sessions" -d "Live agent sessions"
complete -c grace -n "__fish_use_subcommand" -a "setup" -d "Connect CLI agent hooks"
complete -c grace -n "__fish_use_subcommand" -a "doctor" -d "Run health diagnostics"
complete -c grace -n "__fish_use_subcommand" -a "prune" -d "Prune missing folders"
complete -c grace -n "__fish_use_subcommand" -a "stats" -d "Show usage statistics"
complete -c grace -n "__fish_use_subcommand" -a "hub" -d "Open tmux workspace hub"
complete -c grace -n "__fish_use_subcommand" -a "diff" -d "Git diff view"
complete -c grace -n "__fish_use_subcommand" -a "tree" -d "File tree view"
complete -c grace -n "__fish_use_subcommand" -a "init" -d "Print shell init code"
complete -c grace -n "__fish_use_subcommand" -a "completions" -d "Print completion script"
complete -c grace -n "__fish_seen_subcommand_from init completions" -a "zsh bash fish powershell pwsh"
complete -c grace -n "__fish_seen_subcommand_from setup" -a "claude codex gemini qwen copilot opencode agy crush goose amp aider"
complete -c grace -n "__fish_seen_subcommand_from setup" -l all -d "Setup all detected agents"
complete -c grace -n "__fish_seen_subcommand_from setup" -l remove -d "Remove hook"
complete -c grace -n "__fish_seen_subcommand_from import" -a "all claude vscode cursor windsurf antigravity zed t3code"
complete -c grace -n "__fish_seen_subcommand_from doctor" -l fix -d "Auto-fix safe issues"
complete -c grace -n "__fish_seen_subcommand_from prune" -l dry-run -d "Preview missing folders"
complete -c grace -n "__fish_seen_subcommand_from prune" -l older-than -x -d "Only folders unused for N days"
complete -c grace -n "__fish_seen_subcommand_from list" -s t -l tag -x -d "Filter by tool tag"
complete -c grace -n "__fish_seen_subcommand_from list" -l json -d "Output JSON"
complete -c grace -n "__fish_seen_subcommand_from pick" -s t -l tag -x -d "Filter by tool tag"
complete -c grace -n "__fish_seen_subcommand_from pick" -s o -l open -d "Launch last AI tool"
complete -c grace -n "__fish_seen_subcommand_from sessions" -s l -l list -d "Print plain table"
complete -c grace -n "__fish_seen_subcommand_from sessions" -l json -d "Output JSON"
complete -c grace -n "__fish_seen_subcommand_from sessions" -s o -l open -d "Launch last AI tool"
complete -c grace -n "__fish_seen_subcommand_from tool; and not __fish_seen_subcommand_from add rm list" -a "add" -d "Add custom tool"
complete -c grace -n "__fish_seen_subcommand_from tool; and not __fish_seen_subcommand_from add rm list" -a "rm" -d "Remove custom tool"
complete -c grace -n "__fish_seen_subcommand_from tool; and not __fish_seen_subcommand_from add rm list" -a "list" -d "List tools"
complete -c grace -n "__fish_seen_subcommand_from tool; and __fish_seen_subcommand_from add" -l tag -x -d "Tag name for tool"
complete -c grace -n "__fish_seen_subcommand_from tool; and __fish_seen_subcommand_from add" -l launch -x -d "Launch command"
complete -c grace -n "__fish_seen_subcommand_from hub" -a "quit" -d "Stop hub"
complete -c grace -n "__fish_seen_subcommand_from pin unpin forget diff tree hub" -a "(__fish_complete_directories)"
"#;

const POWERSHELL_COMPLETIONS: &str = r#"Register-ArgumentCompleter -Native -CommandName grace -ScriptBlock {
  param($wordToComplete, $commandAst, $cursorPosition)
  $cmds = @('pick','list','pin','unpin','forget','import','tool','sessions','setup','doctor','prune','stats','hub','diff','tree','init','completions')
  $elems = @($commandAst.CommandElements | ForEach-Object { $_.ToString() })
  $sub = $null
  $subIdx = -1
  $skipNext = $false
  for ($i = 1; $i -lt $elems.Count; $i++) {
    if ($skipNext) { $skipNext = $false; continue }
    $t = $elems[$i]
    if ($t -eq '--full' -or $t -eq '-o' -or $t -eq '--open' -or $t -like '--tag=*') { continue }
    if ($t -ceq '-t' -or $t -ceq '--tag') { $skipNext = $true; continue }
    if ($t.StartsWith('-')) { continue }
    $sub = $t
    $subIdx = $i
    break
  }
  if ($skipNext -and $wordToComplete -ne '') { return }
  $typingSub = ($null -eq $sub) -or ($subIdx -eq ($elems.Count - 1) -and $wordToComplete -ne '')
  if ($typingSub) {
    ($cmds + @('-t','--tag','-o','--open','--full','-h','--help','-V','--version')) | Where-Object { $_ -like "$wordToComplete*" } | ForEach-Object {
      [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
    }
    return
  }
  $subArgs = if ($subIdx + 1 -lt $elems.Count) { @($elems[($subIdx + 1)..($elems.Count - 1)]) } else { @() }
  if ($sub -in 'pin','unpin','forget','diff','tree') {
    Get-ChildItem -Directory -ErrorAction SilentlyContinue | Where-Object { $_.Name -like "$wordToComplete*" } | ForEach-Object {
      [System.Management.Automation.CompletionResult]::new($_.Name, $_.Name, 'ProviderContainer', $_.Name)
    }
    return
  }
  $opts = switch ($sub) {
    'init'        { @('zsh','bash','fish','powershell','pwsh') }
    'completions' { @('zsh','bash','fish','powershell','pwsh') }
    'setup'       { @('--all','--remove','claude','codex','gemini','qwen','copilot','opencode','agy','crush','goose','amp','aider') }
    'import'      { @('all','claude','vscode','cursor','windsurf','antigravity','zed','t3code') }
    'doctor'      { @('--fix') }
    'prune'       { @('--dry-run','--older-than') }
    'list'        { @('-t','--tag','--json') }
    'pick'        { @('-t','--tag','-o','--open') }
    'sessions'    { @('-l','--list','--json','-o','--open') }
    'hub'         { @('quit') }
    'tool'        { if ('add' -cin $subArgs) { @('--tag','--launch') } else { @('add','rm','list') } }
    default       { @() }
  }
  $opts | Where-Object { $_ -like "$wordToComplete*" } | ForEach-Object {
    [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
  }
  if ($sub -eq 'hub') {
    Get-ChildItem -Directory -ErrorAction SilentlyContinue | Where-Object { $_.Name -like "$wordToComplete*" } | ForEach-Object {
      [System.Management.Automation.CompletionResult]::new($_.Name, $_.Name, 'ProviderContainer', $_.Name)
    }
  }
}
"#;

#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(clippy::enum_variant_names)]
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
    template
        .replace("@TOOLS@", &joined)
        .replace("@AI@", &ai_joined)
}

/// Print shell tab-completion script for `shell`.
pub fn completions(shell: Shell) -> &'static str {
    match shell {
        Shell::Zsh => ZSH_COMPLETIONS,
        Shell::Bash => BASH_COMPLETIONS,
        Shell::Fish => FISH_COMPLETIONS,
        Shell::PowerShell => POWERSHELL_COMPLETIONS,
    }
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
        assert!(s.contains("GRACE_SHELL=zsh"));
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
        assert!(
            ai_pat.contains("claude|") && ai_pat.contains("kiro"),
            "{ai_pat}"
        );
        assert!(
            !ai_pat.contains("zed") && !ai_pat.contains("nvim"),
            "{ai_pat}"
        );
        let f = script(Shell::Fish);
        assert!(f.contains("session start") && f.contains("fish_postexec"));
        assert!(f.contains("contains -- $base claude"));
        assert!(!f.contains("contains -- $base claude agy antigravity codex gemini cursor cursor-agent opencode aider copilot amp goose crush qwen t3 code"));
        assert!(f.contains("test $n -gt 0"));
    }

    #[test]
    fn sessions_routing_in_every_shell() {
        for sh in [Shell::Zsh, Shell::Bash, Shell::Fish, Shell::PowerShell] {
            let s = script(sh);
            assert!(s.contains("sessions"), "{sh:?}");
            assert!(s.contains("--list"), "{sh:?}");
            assert!(s.contains("--json"), "{sh:?}");
            assert!(s.contains("--full"), "{sh:?}");
            assert!(s.contains("setup"), "{sh:?}");
            assert!(s.contains("session"), "{sh:?}");
            assert!(s.contains("doctor"), "{sh:?}");
            assert!(s.contains("prune"), "{sh:?}");
            assert!(s.contains("stats"), "{sh:?}");
            assert!(s.contains("completions"), "{sh:?}");
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
    fn hub_diff_tree_pass_through() {
        assert!(script(Shell::Zsh).contains("session|hub|diff|tree|help"));
        assert!(script(Shell::Bash).contains("session|hub|diff|tree|help"));
        assert!(script(Shell::Fish).contains("session hub diff tree help"));
        assert!(script(Shell::PowerShell).contains("'session', 'hub', 'diff', 'tree', 'help'"));
    }

    #[test]
    fn parses_shell_names_and_completions() {
        assert_eq!(Shell::parse("pwsh"), Some(Shell::PowerShell));
        assert_eq!(Shell::parse("bash"), Some(Shell::Bash));
        assert_eq!(Shell::parse("nu"), None);
        for sh in [Shell::Zsh, Shell::Bash, Shell::Fish, Shell::PowerShell] {
            let c = completions(sh);
            assert!(c.contains("doctor"), "{sh:?}");
            assert!(c.contains("--tag") || c.contains("-l tag"), "{sh:?}");
            assert!(c.contains("--open") || c.contains("-l open"), "{sh:?}");
            assert!(c.contains("pick"), "{sh:?}");
            assert!(c.contains("--launch") || c.contains("-l launch"), "{sh:?}");
        }
    }

    #[test]
    fn safety_invariants_in_templates() {
        let bash = script(Shell::Bash);
        assert!(
            !bash.contains("eval \"words="),
            "bash must not eval history"
        );
        let zsh = script(Shell::Zsh);
        assert!(zsh.contains("session start --tag --pid $$"));
        assert!(!zsh.contains("${tgt/#\\~/$HOME}"));
        let fish = script(Shell::Fish);
        assert!(fish.contains("session start --tag --pid $fish_pid"));
        let ps = script(Shell::PowerShell);
        assert!(ps.contains("function global:__GraceResolveExe"));
        for sh in [Shell::Zsh, Shell::Bash, Shell::Fish, Shell::PowerShell] {
            let s = script(sh);
            assert!(
                s.contains("[lhV]"),
                "{sh:?} should pass through sessions -lo"
            );
        }
    }
}
