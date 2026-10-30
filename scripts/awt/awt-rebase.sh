#!/usr/bin/env bash
# Waymaker Agent Worktree Rebase Handler
# Usage: awt-rebase.sh <branch> <worktree_path> <base_branch>

branch_raw="$1"
wt_path="$2"
base_raw="$3"

usage() { echo "Usage: awt-rebase.sh <branch> <worktree_path> <base_branch|-> [-i|--confirm]"; }
# A flag where a branch belongs (`awt rebase --help`) must never start a rebase; `-` alone means
# the branch's configured base.
for a in "$branch_raw" "$base_raw"; do
    case "$a" in
        -h|--help) usage; exit 0 ;;
        -) ;;
        -*) echo "awt-rebase.sh: '$a' is not a branch; nothing was rebased (--help)" >&2; exit 2 ;;
    esac
done

branch=$(echo "$branch_raw" | sed -E 's/^[^a-zA-Z0-9._/-]+//; s/[[:space:]].*//')
base=$(echo "$base_raw" | sed -E 's/^[^a-zA-Z0-9._/-]+//; s/[[:space:]].*//')

base="${base:--}"
if [[ "$base" == "-" || -z "$base" ]]; then
    base=$(git -C "$wt_path" config "branch.${branch}.base" 2>/dev/null)
    base="${base:-main}"
fi

# 1. Validation
if [[ "$branch" == "$base" ]]; then
    printf "\n\033[1;31m󰅖 Cannot rebase '%s' onto itself!\033[0m\n" "$branch" >/dev/tty
    sleep 1.2
    exit 0
fi

if [[ "$branch" == "main" || "$branch" == "master" ]]; then
    printf "\n\033[1;31m󰅖 Cannot rebase default base branch '%s'!\033[0m\n" "$branch" >/dev/tty
    sleep 1.2
    exit 0
fi

confirm_prompt=0
for arg in "$@"; do
    case "$arg" in
        --confirm|-i) confirm_prompt=1 ;;
        -) ;;
        -*) echo "awt-rebase.sh: unknown flag '$arg'; nothing was rebased (--help)" >&2; exit 2 ;;
    esac
done

# 1.1 Confirmation prompt
if [[ $confirm_prompt -eq 1 ]]; then
    if command -v gum >/dev/null 2>&1; then
        if ! gum confirm --prompt.foreground="220" "Rebase '$branch' onto '$base'?"; then
            exit 0
        fi
    else
        read -r -p "Rebase '$branch' onto '$base'? [y/N] " confirm </dev/tty
        if [[ "$confirm" != [yY]* ]]; then
            exit 0
        fi
    fi
fi

# 2. Safe Auto-Stash if dirty
stashed=0
dirty_count=$(git -C "$wt_path" status --porcelain 2>/dev/null | wc -l)
if [[ "$dirty_count" -gt 0 ]]; then
    if git -C "$wt_path" stash push -u -m "awt-rebase-autostash: $branch" >/dev/null 2>&1; then
        stashed=1
    fi
fi

# 3. Perform Rebase
git -C "$wt_path" fetch origin "$base" >/dev/null 2>&1 || true

if git -C "$wt_path" rebase "$base" >/dev/null 2>&1; then
    if [[ $stashed -eq 1 ]]; then
        git -C "$wt_path" stash pop >/dev/null 2>&1 || true
    fi
    exit 0
else
    printf "\n\033[1;33m󰀪 Rebase stopped with conflicts in '%s'. Inspect and run 'git rebase --continue'.\033[0m\n" "$branch" >/dev/tty
    sleep 2.5
    exit 1
fi
