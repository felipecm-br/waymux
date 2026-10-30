#!/usr/bin/env bash
# Waymaker Agent Worktree Delete Handler: deletes worktree, kills Tmux session, and switches to last session if active
# Args: <session_name> <wt_path> <branch_raw> [flags...]

session_name="$1"
wt_path="$2"
branch_raw="$3"
shift 3 2>/dev/null || true

usage() {
    echo "Usage: awt-delete.sh <session> <worktree_path> <branch> [-f|--force] [--no-delete-branch] [--no-tmux] [--no-hooks] [-i]"
}
# A flag where the branch belongs (`awt rm --help`) must never delete anything.
case "$branch_raw" in
    -h|--help) usage; exit 0 ;;
    -*) echo "awt-delete.sh: '$branch_raw' is not a branch; nothing was deleted (--help)" >&2; exit 2 ;;
esac

wt_path="${wt_path/#\~/$HOME}"

# Parse flags
force=0
keep_branch=0
no_tmux=0
no_hooks=0
confirm_prompt=0

for arg in "$@"; do
    case "$arg" in
        --confirm|-i) confirm_prompt=1 ;;
        -f|--force) force=1 ;;
        --no-delete-branch) keep_branch=1 ;;
        --no-tmux) no_tmux=1 ;;
        --no-hooks|--skip-pre-remove|-H) no_hooks=1 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "awt-delete.sh: unknown flag '$arg'; nothing was deleted (--help)" >&2; exit 2 ;;
    esac
done

# 1. Resolve exact branch name from worktree HEAD before removal if possible
branch_clean=$(echo "$branch_raw" | sed -E 's/^[^a-zA-Z0-9._/-]+[[:space:]]*//; s/[[:space:]].*//')
if [ -d "$wt_path" ]; then
    detected_branch=$(git -C "$wt_path" rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
    if [[ -n "$detected_branch" && "$detected_branch" != "HEAD" ]]; then
        branch_clean="$detected_branch"
    fi
fi

# 2. Validation: Prevent deleting main branch / root repository
if [[ "$branch_clean" == "main" || "$branch_clean" == "master" ]]; then
    printf "\n\033[1;31m󰅖 Cannot delete default base branch '%s'!\033[0m\n" "$branch_clean" >/dev/tty
    sleep 1.2
    exit 0
fi

# 2b. Confirmation prompt before deletion
if [[ $confirm_prompt -eq 1 && $force -eq 0 ]]; then
    if command -v gum >/dev/null 2>&1; then
        if ! gum confirm --prompt.foreground="196" "Delete worktree '$branch_clean' and kill session '$session_name'?"; then
            exit 0
        fi
    else
        read -r -p "Delete worktree '$branch_clean' and kill session '$session_name'? [y/N] " confirm </dev/tty
        if [[ "$confirm" != [yY]* ]]; then
            exit 0
        fi
    fi
fi

# 3. Check if currently attached to this session in Tmux
cur_session=$(tmux display-message -p '#{session_name}' 2>/dev/null)
is_current_session=0
if [[ -n "$cur_session" && ( "$cur_session" == "$session_name" || "$cur_session" == "_$session_name" ) ]]; then
    is_current_session=1
fi

# 4. Resolve common git dir before unlinking worktree
common_git_dir=$(git -C "$wt_path" rev-parse --git-common-dir 2>/dev/null || git rev-parse --git-common-dir 2>/dev/null || echo "")
if [[ -n "$common_git_dir" && "$common_git_dir" != /* ]]; then
    common_git_dir="$(cd "$wt_path/$common_git_dir" 2>/dev/null && pwd)"
fi

# 4b. Pre-Remove Lifecycle Hook (e.g. archiving artifacts or sanity checks)
if [[ $no_hooks -eq 0 && -d "$wt_path" ]]; then
    pre_remove_hook=""
    if [[ -x "$wt_path/.hooks/pre-remove" ]]; then
        pre_remove_hook="$wt_path/.hooks/pre-remove"
    elif [[ -x "$wt_path/.awt/hooks/pre-remove" ]]; then
        pre_remove_hook="$wt_path/.awt/hooks/pre-remove"
    elif [[ -x "$wt_path/.git/hooks/pre-worktree-remove" ]]; then
        pre_remove_hook="$wt_path/.git/hooks/pre-worktree-remove"
    elif [[ -x "$HOME/.config/waymaker/hooks/pre-remove.sh" ]]; then
        pre_remove_hook="$HOME/.config/waymaker/hooks/pre-remove.sh"
    elif [[ -x "$HOME/.config/matchmaker/hooks/pre-remove.sh" ]]; then
        pre_remove_hook="$HOME/.config/matchmaker/hooks/pre-remove.sh"
    fi

    if [[ -n "$pre_remove_hook" ]]; then
        if ! "$pre_remove_hook" "$wt_path" "$branch_clean"; then
            if [[ $force -eq 0 ]]; then
                printf "\n\033[1;31m󰅖 Pre-remove hook failed! Worktree removal aborted.\033[0m\n" >/dev/tty
                sleep 2
                exit 1
            fi
        fi
    fi
fi

# 5. Atomic Quarantine Trash-Rename & Asynchronous Background Purge (0.1ms UI latency)
if [[ -d "$wt_path" ]]; then
    trash_dir="$(dirname "$wt_path")/.awt_trash_$(basename "$wt_path")_$$_${RANDOM}"
    if mv "$wt_path" "$trash_dir" 2>/dev/null; then
        if [[ -n "$common_git_dir" ]]; then
            git --git-dir="$common_git_dir" worktree prune >/dev/null 2>&1 || true
        else
            git worktree prune >/dev/null 2>&1 || true
        fi
        (rm -rf "$trash_dir" >/dev/null 2>&1 &)
    else
        if [[ -n "$common_git_dir" ]]; then
            git --git-dir="$common_git_dir" worktree remove --force "$wt_path" >/dev/null 2>&1 || rm -rf "$wt_path" 2>/dev/null || true
            git --git-dir="$common_git_dir" worktree prune >/dev/null 2>&1 || true
        else
            git worktree remove -f "$wt_path" >/dev/null 2>&1 || rm -rf "$wt_path" 2>/dev/null || true
        fi
    fi
fi

# 6. Delete Git branch unless --no-delete-branch was specified
if [[ $keep_branch -eq 0 && -n "$branch_clean" ]]; then
    if [[ -n "$common_git_dir" ]]; then
        git --git-dir="$common_git_dir" branch -D "$branch_clean" >/dev/null 2>&1 || true
    else
        git branch -D "$branch_clean" >/dev/null 2>&1 || true
    fi
fi

# 7. Handle Tmux session cleanup & redirection unless --no-tmux was specified
if [[ $no_tmux -eq 0 ]]; then
    if [[ $is_current_session -eq 1 ]]; then
        # Switch to previous session before killing current session
        if command -v wm >/dev/null 2>&1; then
            wm last >/dev/null 2>&1 || tmux switch-client -l >/dev/null 2>&1 || tmux switch-client -n >/dev/null 2>&1
        elif command -v sesh >/dev/null 2>&1; then
            sesh last >/dev/null 2>&1 || tmux switch-client -l >/dev/null 2>&1 || tmux switch-client -n >/dev/null 2>&1
        else
            tmux switch-client -l >/dev/null 2>&1 || tmux switch-client -n >/dev/null 2>&1
        fi
        # Kill the deleted session
        tmux kill-session -t "$session_name" >/dev/null 2>&1 || tmux kill-session -t "_$session_name" >/dev/null 2>&1 || true
        exit 0
    else
        # If the session was in background, kill it cleanly
        if tmux has-session -t "$session_name" >/dev/null 2>&1; then
            tmux kill-session -t "$session_name" >/dev/null 2>&1 || true
        elif tmux has-session -t "_$session_name" >/dev/null 2>&1; then
            tmux kill-session -t "_$session_name" >/dev/null 2>&1 || true
        fi
    fi
fi
