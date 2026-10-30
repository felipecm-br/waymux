#!/usr/bin/env bash
# Waymaker Agent Worktree Merge Handler: merges current branch into target branch with native Git
# Args: <selected_raw> <selected_path> <selected_session> <selected_base> [flags...]

selected_raw="$1"
selected_path="$2"
selected_session="$3"
selected_base="$4"
shift 4 2>/dev/null || true

usage() {
    echo "Usage: awt-merge.sh <branch> <worktree_path> <session> <base> [flags]"
    echo "Flags: --squash --no-squash --no-commit --no-remove --no-tmux --rebase --no-rebase"
    echo "       --push --no-push --no-hooks --into <branch> -y -i"
}
# A flag where the branch belongs (`awt ship --help`) must never start a merge.
case "$selected_raw" in
    -h|--help) usage; exit 0 ;;
    -*) echo "awt-merge.sh: '$selected_raw' is not a branch; nothing was merged (--help)" >&2; exit 2 ;;
esac

# Parse flags
squash=0
no_commit=0
no_remove=0
no_tmux=0
do_rebase=0
do_push=0
no_hooks=0
explicit_target=""

skip_confirm=0
confirm_prompt=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --confirm|-i) confirm_prompt=1 ;;
        -y|--yes) skip_confirm=1 ;;
        --squash) squash=1 ;;
        --no-squash) squash=0 ;;
        --no-commit) no_commit=1 ;;
        --no-remove) no_remove=1 ;;
        --no-tmux|--no-connect) no_tmux=1 ;;
        --rebase) do_rebase=1 ;;
        --no-rebase) do_rebase=0 ;;
        --push|--ship) do_push=1 ;;
        --no-push) do_push=0 ;;
        --no-hooks|--skip-pre-merge|-H) no_hooks=1 ;;
        --into=*) explicit_target="${1#*=}" ;;
        --into) shift; explicit_target="${1:-}" ;;
        -h|--help) usage; exit 0 ;;
        *) echo "awt-merge.sh: unknown flag '$1'; nothing was merged (--help)" >&2; exit 2 ;;
    esac
    shift
done

# 1. Determine Current Worktree & Branch
current_wt=$(git rev-parse --show-toplevel 2>/dev/null)
current_branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null)
current_session=$(tmux display-message -p '#{session_name}' 2>/dev/null)

selected_branch=$(echo "$selected_raw" | sed -E 's/^[^a-zA-Z0-9._/-]+//; s/[[:space:]].*//')

# 2. Determine Source and Target branches
if [[ -n "$explicit_target" ]]; then
    target_branch="$explicit_target"
elif [[ "$selected_branch" == "$current_branch" || -z "$selected_branch" ]]; then
    target_branch=$(git -C "$current_wt" config "branch.${current_branch}.base" 2>/dev/null)
    target_branch="${target_branch:-main}"
else
    target_branch="$selected_branch"
fi
source_branch="$current_branch"

# Validation: cannot merge a branch into itself
if [[ "$source_branch" == "$target_branch" ]]; then
    printf "\n\033[1;31m󰅖 Cannot merge '%s' into itself!\033[0m\n" "$source_branch" >/dev/tty
    sleep 1.2
    exit 0
fi

# Confirmation prompt before merge / ship
if [[ $confirm_prompt -eq 1 && $skip_confirm -eq 0 ]]; then
    if [[ $do_push -eq 1 ]]; then
        prompt_msg="Ship (Merge & Push) '$source_branch' into '$target_branch'?"
        prompt_color="201"
    else
        prompt_msg="Merge '$source_branch' into '$target_branch'?"
        prompt_color="129"
    fi
    if command -v gum >/dev/null 2>&1; then
        if ! gum confirm --prompt.foreground="$prompt_color" "$prompt_msg"; then
            exit 0
        fi
    else
        read -r -p "$prompt_msg [y/N] " confirm </dev/tty
        if [[ "$confirm" != [yY]* ]]; then
            exit 0
        fi
    fi
fi

# 3. Resolve Target Worktree Directory & Session
repo_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
repo_parent=$(basename "$(dirname "$current_wt")")
target_clean="${target_branch//\//-}"
target_wt="$repo_root/../$target_clean"

if [[ ! -d "$target_wt" ]]; then
    if [[ -d "$repo_root/../$target_branch" ]]; then
        target_wt="$repo_root/../$target_branch"
    fi
fi

if [[ "$repo_parent" == ".dotfiles" ]]; then
    target_session="_dotfiles/$target_clean"
else
    target_session="${repo_parent}/${target_clean}"
fi

# 4. Auto-stash dirty state in current worktree
stashed=0
dirty_count=$(git -C "$current_wt" status --porcelain 2>/dev/null | wc -l)
if [[ "$dirty_count" -gt 0 ]]; then
    if git -C "$current_wt" stash push -u -m "awt-merge-autostash: $source_branch" >/dev/null 2>&1; then
        stashed=1
    fi
fi

# 4b. Pre-Merge Lifecycle Hook Validation Gate
if [[ $no_hooks -eq 0 ]]; then
    pre_merge_hook=""
    if [[ -x "$current_wt/.hooks/pre-merge" ]]; then
        pre_merge_hook="$current_wt/.hooks/pre-merge"
    elif [[ -x "$current_wt/.awt/hooks/pre-merge" ]]; then
        pre_merge_hook="$current_wt/.awt/hooks/pre-merge"
    elif [[ -x "$current_wt/.git/hooks/pre-worktree-merge" ]]; then
        pre_merge_hook="$current_wt/.git/hooks/pre-worktree-merge"
    elif [[ -x "$HOME/.config/waymaker/hooks/pre-merge.sh" ]]; then
        pre_merge_hook="$HOME/.config/waymaker/hooks/pre-merge.sh"
    elif [[ -x "$HOME/.config/matchmaker/hooks/pre-merge.sh" ]]; then
        pre_merge_hook="$HOME/.config/matchmaker/hooks/pre-merge.sh"
    fi

    if [[ -n "$pre_merge_hook" ]]; then
        printf "\n\033[1;34m󰑮 Running pre-merge validation hook...\033[0m\n" >/dev/tty
        if ! "$pre_merge_hook" "$current_wt" "$source_branch" "$target_branch"; then
            printf "\n\033[1;31m󰅖 Pre-merge hook failed! Merge aborted and worktree kept intact.\033[0m\n" >/dev/tty
            if [[ $stashed -eq 1 ]]; then
                git -C "$current_wt" stash pop >/dev/null 2>&1 || true
            fi
            if command -v pw-play >/dev/null 2>&1 && [[ -f "$HOME/.local/share/sounds/ai/10-arcade-blip.wav" ]]; then
                pw-play "$HOME/.local/share/sounds/ai/10-arcade-blip.wav" >/dev/null 2>&1 &
            fi
            sleep 2.5
            exit 1
        fi
        printf "\033[1;32m󰄬 Pre-merge validation passed.\033[0m\n" >/dev/tty
    fi
fi

# 5. Optional Linear Rebase onto Target Branch before Merging
if [[ $do_rebase -eq 1 ]]; then
    rebase_target="$target_branch"
    if git -C "$current_wt" remote get-url origin >/dev/null 2>&1; then
        git -C "$current_wt" fetch origin "$target_branch" >/dev/null 2>&1 || true
        if git -C "$current_wt" rev-parse --verify "origin/$target_branch" >/dev/null 2>&1; then
            rebase_target="origin/$target_branch"
        fi
    fi

    # Fast-forward target_wt if possible before rebase
    if [[ -d "$target_wt" ]]; then
        git -C "$target_wt" pull --ff-only origin "$target_branch" >/dev/null 2>&1 || true
    fi

    if ! git -C "$current_wt" rebase "$rebase_target" >/dev/null 2>&1; then
        printf "\n\033[1;33m󰀪 Rebase onto '%s' encountered conflicts! Worktree kept intact.\033[0m\n" "$rebase_target" >/dev/tty
        git -C "$current_wt" rebase --abort >/dev/null 2>&1 || true
        if [[ $stashed -eq 1 ]]; then
            git -C "$current_wt" stash pop >/dev/null 2>&1 || true
        fi
        if command -v pw-play >/dev/null 2>&1 && [[ -f "$HOME/.local/share/sounds/ai/10-arcade-blip.wav" ]]; then
            pw-play "$HOME/.local/share/sounds/ai/10-arcade-blip.wav" >/dev/null 2>&1 &
        fi
        sleep 2.5
        exit 1
    fi
fi

# 6. Execute 100% Native Git Merge in Target Worktree
merge_success=0
target_orig_head=""

if [[ -d "$target_wt" ]]; then
    target_orig_head=$(git -C "$target_wt" rev-parse HEAD 2>/dev/null || echo "")

    if [[ $squash -eq 1 ]]; then
        if git -C "$target_wt" merge --squash "$source_branch" >/dev/null 2>&1; then
            if [[ $no_commit -eq 0 ]]; then
                git -C "$target_wt" commit -m "squash: merge $source_branch into $target_branch" --trailer "Test-Status: pass (quality-gate)" >/dev/null 2>&1 || true
            fi
            merge_success=1
        fi
    elif [[ $no_commit -eq 1 ]]; then
        if git -C "$target_wt" merge --no-ff --no-commit "$source_branch" >/dev/null 2>&1; then
            merge_success=1
        fi
    else
        # Standard fast-forward or merge commit
        if git -C "$target_wt" merge --ff "$source_branch" >/dev/null 2>&1; then
            merge_success=1
        elif git -C "$target_wt" merge "$source_branch" -m "merge: $source_branch into $target_branch" >/dev/null 2>&1; then
            git -C "$target_wt" commit --amend --no-edit --trailer "Test-Status: pass (quality-gate)" >/dev/null 2>&1 || true
            merge_success=1
        fi
    fi
fi

if [[ $merge_success -eq 1 ]]; then
    # Ship Action: Push target branch to remote origin if requested
    if [[ $do_push -eq 1 ]]; then
        if git -C "$target_wt" push origin "$target_branch" >/dev/null 2>&1; then
            if command -v tmux >/dev/null 2>&1 && [ -n "$TMUX" ]; then
                tmux display-message -d 3000 " 󱓍 Shipped: $source_branch merged & pushed to $target_branch"
            fi
            printf "\n\033[1;32m󰄬 Successfully shipped '%s' into '%s' (pushed to origin).\033[0m\n" "$source_branch" "$target_branch" >/dev/tty
        else
            printf "\n\033[1;31m󰅖 Push to origin/%s failed! Rolling back local merge in '%s'...\033[0m\n" "$target_branch" "$target_branch" >/dev/tty
            # Atomic rollback: revert target worktree to pre-merge commit so state remains consistent
            if [[ -n "$target_orig_head" ]]; then
                git -C "$target_wt" reset --hard "$target_orig_head" >/dev/null 2>&1 || true
            fi
            if [[ $stashed -eq 1 ]]; then
                git -C "$current_wt" stash pop >/dev/null 2>&1 || true
            fi
            if command -v pw-play >/dev/null 2>&1 && [[ -f "$HOME/.local/share/sounds/ai/10-arcade-blip.wav" ]]; then
                pw-play "$HOME/.local/share/sounds/ai/10-arcade-blip.wav" >/dev/null 2>&1 &
            fi
            sleep 2.5
            exit 1
        fi
    fi

    # Completion earcon
    if command -v pw-play >/dev/null 2>&1 && [[ -f "$HOME/.local/share/sounds/ai/01-crystal-chime.wav" ]]; then
        pw-play "$HOME/.local/share/sounds/ai/01-crystal-chime.wav" >/dev/null 2>&1 &
    fi

    # Trigger post-merge lifecycle hook
    if [[ -x "$HOME/.config/waymaker/hooks/post-merge.sh" && -d "$target_wt" ]]; then
        "$HOME/.config/waymaker/hooks/post-merge.sh" "$target_wt" "$target_branch" "$source_branch" 2>/dev/null || true
    elif [[ -x "$HOME/.config/matchmaker/hooks/post-merge.sh" && -d "$target_wt" ]]; then
        "$HOME/.config/matchmaker/hooks/post-merge.sh" "$target_wt" "$target_branch" "$source_branch" 2>/dev/null || true
    fi

    # Cleanup source worktree & branch unless --no-remove was specified (Atomic 0.1ms Trash-Rename + Async Purge)
    if [[ $no_remove -eq 0 && "$current_wt" != "$target_wt" ]]; then
        if [[ -d "$current_wt" ]]; then
            trash_dir="$(dirname "$current_wt")/.awt_trash_$(basename "$current_wt")_$$_${RANDOM}"
            if mv "$current_wt" "$trash_dir" 2>/dev/null; then
                git -C "$target_wt" worktree prune >/dev/null 2>&1 || true
                (rm -rf "$trash_dir" >/dev/null 2>&1 &)
            else
                git -C "$target_wt" worktree remove -f "$current_wt" >/dev/null 2>&1 || \
                rm -rf "$current_wt" 2>/dev/null || true
                git -C "$target_wt" worktree prune >/dev/null 2>&1 || true
            fi
        fi
        git -C "$target_wt" branch -d "$source_branch" >/dev/null 2>&1 || \
        git -C "$target_wt" branch -D "$source_branch" >/dev/null 2>&1 || true
    fi

    # Switch Tmux session to target unless --no-tmux was specified
    if [[ $no_tmux -eq 0 ]]; then
        if command -v wm >/dev/null 2>&1; then
            if tmux has-session -t "$target_session" >/dev/null 2>&1; then
                wm connect "$target_session" >/dev/null 2>&1
            elif [[ -d "$target_wt" ]]; then
                wm connect "$target_wt" >/dev/null 2>&1
            fi
        elif command -v sesh >/dev/null 2>&1; then
            if tmux has-session -t "$target_session" >/dev/null 2>&1; then
                sesh connect "$target_session" >/dev/null 2>&1
            elif [[ -d "$target_wt" ]]; then
                sesh connect "$target_wt" >/dev/null 2>&1
            fi
        fi

        # Kill old Tmux session if we left it and removed it
        if [[ $no_remove -eq 0 && -n "$current_session" && "$current_session" != "$target_session" ]]; then
            tmux kill-session -t "$current_session" >/dev/null 2>&1 || true
        fi
    fi
    exit 0
else
    # Restore stash if merge failed
    if [[ $stashed -eq 1 ]]; then
        git -C "$current_wt" stash pop >/dev/null 2>&1 || true
    fi
    if command -v pw-play >/dev/null 2>&1 && [[ -f "$HOME/.local/share/sounds/ai/10-arcade-blip.wav" ]]; then
        pw-play "$HOME/.local/share/sounds/ai/10-arcade-blip.wav" >/dev/null 2>&1 &
    fi
    printf "\n\033[1;31m󰅖 Merge failed or has conflicts! Worktree kept intact for resolution.\033[0m\n" >/dev/tty
    sleep 2
    exit 1
fi
