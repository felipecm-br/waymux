#!/usr/bin/env bash
# awt-sweep.sh — Prune merged Git worktrees, clean branches, and terminate associated Tmux sessions
# Usage: awt sweep [-y|--yes] [-n|--dry-run] [--fetch] [base_branch]
set -euo pipefail

R='\033[0m'
BOLD='\033[1m'
DIM='\033[2m'
C_CYAN='\033[38;2;131;192;146m'
C_YELLOW='\033[38;2;219;188;127m'
C_RED='\033[38;2;230;126;128m'
C_BLUE='\033[38;2;127;187;179m'
C_GRAY='\033[38;2;114;135;152m'

auto_yes=0
dry_run=0
do_fetch=0
scan_all=0
clean_targets=0
base_branch_arg=""
idle_arg=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    -y|--yes) auto_yes=1 ;;
    -n|--dry-run) dry_run=1 ;;
    --fetch) do_fetch=1 ;;
    -a|--all) scan_all=1 ;;
    --clean-targets) clean_targets=1 ;;
    --idle) idle_arg="${2:-}"; shift ;;
    --idle=*) idle_arg="${1#--idle=}" ;;
    -h|--help)
      cat << 'HELP'
awt sweep — Clean up merged worktrees and stale sibling build artifacts

USAGE:
    awt sweep [flags] [base_branch]

FLAGS:
    -a, --all             Scan every repository under $AWT_SWEEP_ROOTS
                          (colon-separated; default ~/.dotfiles:~/dev/github)
    --clean-targets       Clean Rust target/ directories in secondary sibling worktrees
    -y, --yes             Confirm and remove all merged worktrees without interactive prompt
    -n, --dry-run         List worktrees and targets that would be swept without deleting
    --idle DAYS           Also list clean worktrees with no commit or index change for DAYS
                          days and no tmux pane inside them (0 turns it off)

PER-REPOSITORY RULES (git config, idea from Orca 1.4.259's workspace cleanup rules):
    awt.sweepKeep         Space-separated branch globs never swept, e.g. "spike/* demo"
    awt.sweepIdleDays     Default for --idle in this repository (unset or 0: merged only)

Every removal goes through awt rm, which backs up unmerged or uncommitted work to
refs/awt-backup/<branch> first; a worktree it cannot remove is kept, never purged.
    --fetch               Fetch origin before checking branch merge status
    -h, --help            Show this help message
HELP
      exit 0
      ;;
    *)
      if [[ -z "$base_branch_arg" ]]; then
        base_branch_arg="$1"
      fi
      ;;
  esac
  shift
done

declare -a repo_roots=()
# Long-lived personal branches whose target/ is never cleaned (space-separated).
keep_branches="${AWT_SWEEP_KEEP_BRANCHES:-$(id -un)}"

# Adds a repository directory (or its main/master worktree) to repo_roots; returns 1 if none.
add_repo_root() {
  local d="$1"
  if [[ -d "$d/main" ]] && git -C "$d/main" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    repo_roots+=("$d/main")
  elif [[ -d "$d/master" ]] && git -C "$d/master" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    repo_roots+=("$d/master")
  elif git -C "$d" rev-parse --is-inside-work-tree >/dev/null 2>&1 || git -C "$d" rev-parse --is-bare-repository >/dev/null 2>&1; then
    repo_roots+=("$d")
  else
    return 1
  fi
}

if [[ $scan_all -eq 1 ]]; then
  real_home="$(getent passwd "$(id -un)" 2>/dev/null | cut -d: -f6 || echo "$HOME")"
  [[ -z "$real_home" ]] && real_home="$HOME"
  # Colon-separated dirs: each is a repository itself or a folder of repositories.
  IFS=':' read -r -a sweep_roots <<< "${AWT_SWEEP_ROOTS:-$real_home/.dotfiles:$real_home/dev/github}"
  for root in "${sweep_roots[@]}"; do
    [[ -d "$root" ]] || continue
    if add_repo_root "$root"; then
      continue
    fi
    for d in "$root"/*; do
      [[ -d "$d" ]] || continue
      add_repo_root "$d" || true
    done
  done
else
  repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
  if ! git -C "$repo_root" rev-parse --is-inside-work-tree >/dev/null 2>&1 && ! git -C "$repo_root" rev-parse --is-bare-repository >/dev/null 2>&1; then
    printf "${C_RED}󰅖 awt sweep: Not inside a Git repository (use --all to scan all repos).${R}\n"
    exit 1
  fi
  repo_roots+=("$repo_root")
fi

# Whether branch $1 matches one of the space-separated globs in $2.
is_kept_branch() {
  local branch="$1" glob
  for glob in $2; do
    # shellcheck disable=SC2053 # $glob is a pattern on purpose
    [[ "$branch" == $glob ]] && return 0
  done
  return 1
}

# Whether worktree $1 is clean (no tracked change, no untracked file), its HEAD commit and its
# index are older than $2 days, and no tmux pane (paths in $3, one per line) is inside it.
is_idle_worktree() {
  local wt="$1" days="$2" live="$3" now head_ts index git_dir idx_ts p
  while IFS= read -r p; do
    [[ -n "$p" && ( "$p" == "$wt" || "$p" == "$wt/"* ) ]] && return 1
  done <<< "$live"
  now=$(date +%s)
  head_ts=$(git -C "$wt" log -1 --format=%ct 2>/dev/null || echo "$now")
  git_dir=$(git -C "$wt" rev-parse --absolute-git-dir 2>/dev/null || echo "")
  idx_ts=0
  index="$git_dir/index"
  [[ -f "$index" ]] && idx_ts=$(stat -c %Y "$index" 2>/dev/null || stat -f %m "$index" 2>/dev/null || echo "$now")
  (( head_ts > idx_ts )) || head_ts=$idx_ts
  (( now - head_ts >= days * 86400 )) || return 1
  # Last: a plain status refreshes the index (its mtime is the age read above); this one does not.
  [[ -z "$(git -C "$wt" --no-optional-locks status --porcelain --untracked-files=normal 2>/dev/null)" ]]
}

declare -a candidate_repos=()
declare -a candidate_paths=()
declare -a candidate_branches=()
declare -a candidate_reasons=()
declare -a target_dirs_to_clean=()
declare -a target_sizes=()

for repo_root in "${repo_roots[@]}"; do
  # Determine base branch for this repo
  if [[ -n "$base_branch_arg" ]]; then
    base_branch="$base_branch_arg"
  else
    base_branch=$(git -C "$repo_root" symbolic-ref refs/remotes/origin/HEAD 2>/dev/null | sed 's@^refs/remotes/origin/@@' || echo "")
    if [[ -z "$base_branch" ]]; then
      if git -C "$repo_root" show-ref --verify --quiet refs/heads/main; then
        base_branch="main"
      elif git -C "$repo_root" show-ref --verify --quiet refs/heads/master; then
        base_branch="master"
      else
        base_branch="main"
      fi
    fi
  fi

  # Per-repository rules.
  keep_globs=$(git -C "$repo_root" config --get awt.sweepKeep 2>/dev/null || true)
  idle_days="${idle_arg:-$(git -C "$repo_root" config --get awt.sweepIdleDays 2>/dev/null || echo 0)}"
  [[ "$idle_days" =~ ^[0-9]+$ ]] || idle_days=0
  live_paths=""
  if [[ $idle_days -gt 0 ]]; then
    live_paths=$(tmux list-panes -a -F '#{pane_current_path}' 2>/dev/null || true)
  fi

  if [[ $do_fetch -eq 1 ]]; then
    printf "${DIM}Fetching origin references in %s...${R}\n" "$repo_root"
    git -C "$repo_root" fetch --prune origin 2>/dev/null || true
  fi

  if [[ $scan_all -eq 0 ]]; then
    printf "${BOLD}󰈈 Scanning worktrees in %s (base: %s)...${R}\n\n" "$(basename "$(dirname "$repo_root")")/$(basename "$repo_root")" "$base_branch"
  fi

  cur_path=""
  cur_head=""
  cur_branch=""
  is_bare=0

  while IFS= read -r line; do
    if [[ "$line" =~ ^worktree[[:space:]]+(.*) ]]; then
      cur_path="${BASH_REMATCH[1]}"
      cur_head=""
      cur_branch=""
      is_bare=0
    elif [[ "$line" == "bare" ]]; then
      is_bare=1
    elif [[ "$line" =~ ^HEAD[[:space:]]+([0-9a-f]+) ]]; then
      cur_head="${BASH_REMATCH[1]}"
    elif [[ "$line" =~ ^branch[[:space:]]+refs/heads/(.*) ]]; then
      cur_branch="${BASH_REMATCH[1]}"
    elif [[ -z "$line" && -n "$cur_path" ]]; then
      if [[ $is_bare -eq 0 && -n "$cur_branch" && "$cur_branch" != "$base_branch" && "$cur_branch" != "master" && "$cur_branch" != "main" ]]; then
        merged=0
        reason=""

        # 1. Check local ancestor
        if git -C "$repo_root" show-ref --verify --quiet "refs/heads/$base_branch" && \
           git -C "$repo_root" merge-base --is-ancestor "refs/heads/$cur_branch" "refs/heads/$base_branch" 2>/dev/null; then
          merged=1
          reason="Merged locally into ${base_branch}"
        # 2. Check remote tracking ancestor
        elif git -C "$repo_root" show-ref --verify --quiet "refs/remotes/origin/$base_branch" && \
             git -C "$repo_root" merge-base --is-ancestor "refs/heads/$cur_branch" "refs/remotes/origin/$base_branch" 2>/dev/null; then
          merged=1
          reason="Merged upstream into origin/${base_branch}"
        # 3. Check GitHub PR state if gh CLI is available (catches squash/rebase merges)
        elif command -v gh >/dev/null 2>&1; then
          pr_state=$(cd "$cur_path" 2>/dev/null && gh pr view "$cur_branch" --json state -q .state 2>/dev/null || echo "")
          if [[ "$pr_state" == "MERGED" ]]; then
            merged=1
            pr_num=$(cd "$cur_path" 2>/dev/null && gh pr view "$cur_branch" --json number -q .number 2>/dev/null || echo "")
            reason="PR #${pr_num} Merged on GitHub"
          fi
        fi

        # 4. Idle: clean, no commit or index change for idle_days, no tmux pane inside it.
        if [[ $merged -eq 0 && $idle_days -gt 0 ]] && is_idle_worktree "$cur_path" "$idle_days" "$live_paths"; then
          merged=1
          reason="Idle ${idle_days}+ days, clean"
        fi

        if [[ $merged -eq 1 ]] && is_kept_branch "$cur_branch" "$keep_globs"; then
          merged=0
          printf "  ${C_GRAY}󰐃 %-28s kept by awt.sweepKeep${R}\n" "$cur_branch"
        fi

        if [[ $merged -eq 1 ]]; then
          candidate_repos+=("$repo_root")
          candidate_paths+=("$cur_path")
          candidate_branches+=("$cur_branch")
          candidate_reasons+=("$reason")
        elif [[ $clean_targets -eq 1 && " $keep_branches " != *" $cur_branch "* && -d "$cur_path/target" && -f "$cur_path/Cargo.toml" ]]; then
          t_sz=$(du -sh "$cur_path/target" 2>/dev/null | awk 'NR==1 {print $1}' || true)
          [[ -z "$t_sz" ]] && t_sz="?"
          target_dirs_to_clean+=("$cur_path/target")
          target_sizes+=("$t_sz ($cur_branch)")
        fi
      fi
      cur_path=""
      cur_head=""
      cur_branch=""
      is_bare=0
    fi
  done < <(git -C "$repo_root" worktree list --porcelain 2>/dev/null || true; printf '\n')
done

total_candidates=${#candidate_paths[@]}
total_targets=${#target_dirs_to_clean[@]}

if [[ $total_candidates -eq 0 && $total_targets -eq 0 ]]; then
  printf "${C_CYAN}✔ No merged worktrees or stale secondary targets found.${R} Everything is clean and synchronized.\n"
  exit 0
fi

if [[ $total_candidates -gt 0 ]]; then
  printf "${C_YELLOW}${BOLD}Found %d worktree(s) eligible for sweep:${R}\n\n" "$total_candidates"
  for i in "${!candidate_paths[@]}"; do
    c_br="${candidate_branches[$i]}"
    c_pt="${candidate_paths[$i]}"
    c_rs="${candidate_reasons[$i]}"
    printf "  ${C_RED}󰆴 %-28s${R} ${C_GRAY}%s${R}  ${C_CYAN}(%s)${R}\n" "$c_br" "$c_pt" "$c_rs"
  done
  printf "\n"
fi

if [[ $total_targets -gt 0 ]]; then
  printf "${C_YELLOW}${BOLD}Found %d secondary worktree Rust target/ folder(s) eligible for cleanup:${R}\n\n" "$total_targets"
  for i in "${!target_dirs_to_clean[@]}"; do
    printf "  ${C_RED}󰆴 %-28s${R} ${C_GRAY}%s${R}\n" "${target_sizes[$i]}" "${target_dirs_to_clean[$i]}"
  done
  printf "\n"
fi

if [[ $dry_run -eq 1 ]]; then
  printf "${DIM}[Dry run mode: no worktrees, sessions, or target/ directories were removed]${R}\n"
  exit 0
fi

# Confirmation prompt
if [[ $auto_yes -eq 0 ]]; then
  if command -v gum >/dev/null 2>&1; then
    if ! gum confirm --prompt.foreground="214" "Proceed with sweeping $total_candidates worktree(s) and $total_targets target/ folder(s)?"; then
      printf "${DIM}Sweep aborted.${R}\n"
      exit 0
    fi
  else
    read -r -p "Proceed with sweeping $total_candidates worktree(s) and $total_targets target/ folder(s)? [y/N] " confirm </dev/tty
    if [[ "$confirm" != [yY]* ]]; then
      printf "${DIM}Sweep aborted.${R}\n"
      exit 0
    fi
  fi
fi

# Execute worktree cleanup
swept_count=0
for i in "${!candidate_paths[@]}"; do
  c_rp="${candidate_repos[$i]}"
  c_br="${candidate_branches[$i]}"
  c_pt="${candidate_paths[$i]}"
  printf "${DIM}Sweeping %s (%s)...${R} " "$c_br" "$c_pt"

  if (cd "$c_rp" && awt rm "$c_br" -f --no-tmux) >/dev/null 2>&1; then
    printf "${C_CYAN}✔ removed${R}\n"
    swept_count=$((swept_count + 1))
  else
    # No purge without awt rm: it is the step that backs the work up first.
    printf "${C_YELLOW}⚠ kept (awt rm failed; run: awt rm %s)${R}\n" "$c_br"
  fi
done

for repo_root in "${repo_roots[@]}"; do
  git -C "$repo_root" worktree prune 2>/dev/null || true
done

# Execute secondary Rust target/ cleanup
cleaned_targets=0
for i in "${!target_dirs_to_clean[@]}"; do
  t_dir="${target_dirs_to_clean[$i]}"
  printf "${DIM}Cleaning %s...${R} " "$t_dir"
  rm -rf "$t_dir" 2>/dev/null || true
  printf "${C_CYAN}✔ cleaned (%s)${R}\n" "${target_sizes[$i]}"
  cleaned_targets=$((cleaned_targets + 1))
done

printf "\n${C_CYAN}${BOLD}✔ Successfully swept %d worktree(s) and cleaned %d secondary target/ folder(s).${R}\n" "$swept_count" "$cleaned_targets"
exit 0
