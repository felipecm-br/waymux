#!/usr/bin/env bash
# Waymaker Agent Worktree PR Handler: interactive PR selector & worktree provisioner
# Usage: awt-pr.sh [pr_number_or_url]

set -e

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1 && ! git rev-parse --is-bare-repository >/dev/null 2>&1; then
    echo "awt-pr: Not inside a Git repository."
    exit 1
fi

if ! command -v gh >/dev/null 2>&1; then
    echo "awt-pr: GitHub CLI (gh) is not installed or not in PATH."
    exit 1
fi

repo_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
pr_num=""
head_branch=""

if [[ $# -gt 0 && -n "$1" ]]; then
    raw_arg="$1"
    # Parse PR number from URL or raw number
    pr_num=$(echo "$raw_arg" | sed -E 's/.*pull\/([0-9]+).*/\1/; s/^#//')
    if ! [[ "$pr_num" =~ ^[0-9]+$ ]]; then
        echo "awt-pr: Invalid PR number '$raw_arg'"
        exit 1
    fi
else
    # Interactive Waymaker PR selector
    if command -v wm >/dev/null 2>&1; then
        MM_TUI_ARGS=()
        if [ "$TMUX_POPUP" = "1" ]; then
            MM_TUI_ARGS=("tui.percentage=100" "tui.max=9999")
        fi
        output=$(wm -o awt-pr "${MM_TUI_ARGS[@]}")
        [[ -z "$output" ]] && exit 0
        IFS=$'\t' read -r pr_raw head_branch <<< "$output"
        pr_num="${pr_raw#\#}"
    else
        echo "awt-pr: Waymaker (wm) not found. Specify PR number directly: awt pr <number>"
        exit 1
    fi
fi

[[ -z "$pr_num" ]] && exit 0

# Resolve target worktree directory and branch name
clean_head=$(echo "$head_branch" | sed -E 's/[^a-zA-Z0-9._/-]+//g; s/\//-/g')
if [[ -n "$clean_head" ]]; then
    branch_name="pr/${pr_num}-${clean_head}"
    target_folder="pr-${pr_num}-${clean_head}"
else
    branch_name="pr/${pr_num}"
    target_folder="pr-${pr_num}"
fi

target_dir="$repo_root/../$target_folder"
target_dir=$(realpath "$target_dir" 2>/dev/null || echo "$target_dir")

repo_parent=$(basename "$(dirname "$repo_root")")
if [[ "$repo_parent" == ".dotfiles" ]]; then
    session_name="_dotfiles/$target_folder"
else
    session_name="${repo_parent}/${target_folder}"
fi

# If worktree already exists, connect directly
if [[ -d "$target_dir" ]]; then
    printf "\n\033[1;36m󰄬 Worktree already exists at %s, connecting...\033[0m\n" "$target_dir"
    if command -v wm >/dev/null 2>&1; then
        if tmux has-session -t "$session_name" 2>/dev/null; then
            exec wm connect "$session_name"
        else
            exec wm connect "$target_dir"
        fi
    elif command -v sesh >/dev/null 2>&1; then
        if tmux has-session -t "$session_name" 2>/dev/null; then
            exec sesh connect "$session_name"
        else
            exec sesh connect "$target_dir"
        fi
    else
        cd "$target_dir"
    fi
    exit 0
fi

printf "\n\033[1;32m󰄬 Provisioning worktree for PR #%s at %s...\033[0m\n" "$pr_num" "$target_dir"

# Fetch PR branch directly into local branch ref
git -C "$repo_root" fetch origin "pull/$pr_num/head:$branch_name" --force 2>/dev/null || \
git fetch origin "pull/$pr_num/head:$branch_name" --force 2>/dev/null || true

# Provision worktree from fetched branch
if ! git worktree add "$target_dir" "$branch_name" 2>/dev/null; then
    # Fallback to creating from HEAD or tracking branch
    git worktree add "$target_dir" -b "$branch_name" 2>/dev/null || {
        printf "\n\033[1;31m󰅖 Failed to create worktree at %s\033[0m\n" "$target_dir"
        exit 1
    }
fi

# Set base branch config
git -C "$target_dir" config "branch.${branch_name}.base" "main" 2>/dev/null || true

# Run post-create hook
hook_script="$HOME/.config/waymaker/hooks/post-create.sh"
[ -x "$hook_script" ] || hook_script="$HOME/.config/matchmaker/hooks/post-create.sh"
[ -x "$hook_script" ] || hook_script="$(dirname "$0")/../hooks/post-create.sh"
if [[ -x "$hook_script" ]]; then
    "$hook_script" "$target_dir" "$branch_name" "main" 2>/dev/null || true
fi

# Dismiss parent tmux popup if active so user lands cleanly in the new session
if [ -n "$TMUX" ]; then
    tmux display-popup -C 2>/dev/null || true
fi

# Connect via Waymaker / Sesh / Tmux
if command -v wm >/dev/null 2>&1; then
    if tmux has-session -t "$session_name" 2>/dev/null; then
        exec wm connect "$session_name"
    else
        exec wm connect "$target_dir"
    fi
elif command -v sesh >/dev/null 2>&1; then
    if tmux has-session -t "$session_name" 2>/dev/null; then
        exec sesh connect "$session_name"
    else
        exec sesh connect "$target_dir"
    fi
elif [ -n "$TMUX" ]; then
    tmux switch-client -t "$session_name" 2>/dev/null || cd "$target_dir"
else
    cd "$target_dir"
fi
