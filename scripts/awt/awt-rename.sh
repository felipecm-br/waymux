#!/usr/bin/env bash
# Waymaker Agent Worktree Rename Handler
# Usage: awt-rename.sh <old_branch> <worktree_path> <new_branch>

old_branch_raw="$1"
wt_path="$2"
new_branch_raw="$3"

old_branch=$(echo "$old_branch_raw" | sed -E 's/^[^a-zA-Z0-9._/-]+//; s/[[:space:]].*//')

if [[ "$old_branch" == "main" || "$old_branch" == "master" ]]; then
    printf "\n\033[1;31m󰅖 Cannot rename default base branch '%s'!\033[0m\n" "$old_branch" >/dev/tty
    sleep 1.2
    exit 0
fi

if [[ -z "$new_branch_raw" && -t 0 ]]; then
    if command -v gum >/dev/null 2>&1; then
        new_branch_raw=$(gum input --prompt=" Rename branch to: " --value="$old_branch" </dev/tty)
    else
        read -r -p "Rename branch to: " new_branch_raw </dev/tty
    fi
fi

new_branch=$(echo "$new_branch_raw" | sed -E 's/^[^a-zA-Z0-9._/-]+//; s/[[:space:]].*//')

# 1. Validation
if [[ -z "$new_branch" || "$new_branch" == "$old_branch" ]]; then
    exit 0
fi


# 1.1 Self-heal worktree path if it was moved or out of sync
if [[ ! -d "$wt_path" ]]; then
    git worktree repair >/dev/null 2>&1 || true
    detected_path=$(git worktree list --porcelain 2>/dev/null | awk -v b="refs/heads/$old_branch" '
        /^worktree / { p = substr($0, 10) }
        $0 == "branch " b { print p; exit }
    ')
    if [[ -n "$detected_path" && -d "$detected_path" ]]; then
        wt_path="$detected_path"
    fi
fi

# 2. Rename branch in Git
if ! git -C "$wt_path" branch -m "$old_branch" "$new_branch" 2>/dev/null; then
    printf "\n\033[1;31m󰅖 Failed to rename branch '%s' to '%s'!\033[0m\n" "$old_branch" "$new_branch" >/dev/tty
    sleep 1.2
    exit 1
fi

# 3. Rename worktree folder if necessary
parent_dir="$(dirname "$wt_path")"
repo_parent="$(basename "$parent_dir")"
old_folder="$(basename "$wt_path")"
new_folder="${new_branch//\//-}"
target_new="$parent_dir/$new_folder"

if [[ -d "$wt_path" && "$wt_path" != "$target_new" && ! -d "$target_new" ]]; then
    if ! git -C "$wt_path" worktree move --force "$wt_path" "$target_new" 2>/dev/null; then
        mv "$wt_path" "$target_new" 2>/dev/null || true
        git -C "$target_new" worktree repair "$target_new" 2>/dev/null || true
    fi
    wt_path="$target_new"
fi

# 4. Rename Tmux session if active or present
if [[ "$repo_parent" == ".dotfiles" ]]; then
    old_session="_dotfiles/$old_folder"
    new_session="_dotfiles/$new_folder"
else
    old_session="${repo_parent}/${old_folder}"
    new_session="${repo_parent}/${new_folder}"
fi

if tmux has-session -t "$old_session" 2>/dev/null; then
    tmux rename-session -t "$old_session" "$new_session" 2>/dev/null || true
fi

exit 0
