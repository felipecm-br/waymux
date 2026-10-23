#!/usr/bin/env bash
# 100% Waymaker Presets Worktree Creation Wizard with AI Conversation Continuation
trap 'exit 0' HUP INT TERM

SCRIPT_DIR=$(dirname "$(readlink -f "$0" 2>/dev/null || realpath "$0")")
# shellcheck source=/dev/null
. "$SCRIPT_DIR/awt-ai-detect.sh" 2>/dev/null || true

step=1
icon="󰓹"
prefix=""
slug=""
branch_name=""
bbase=""
ai_cmd="__FRESH__"

MM_TUI_ARGS=()
if [ "$TMUX_POPUP" = "1" ]; then
    MM_TUI_ARGS=("tui.percentage=100" "tui.max=9999")
fi

selected_base="$(echo "${1:-}" | sed -E 's/^[^a-zA-Z0-9._/-]+[[:space:]]*//')"

connect_tmux=1
ai_continue_mode="auto"
custom_ai_cmd=""
WM_BIN="wm"

shift 2>/dev/null || true
while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-tmux|--no-connect)
            connect_tmux=0
            ;;
        --continue|--ai-continue)
            ai_continue_mode="yes"
            ;;
        --no-continue|--fresh)
            ai_continue_mode="no"
            ;;
        --ai=*)
            ai_continue_mode="custom"
            custom_ai_cmd="${1#*=}"
            ;;
        --)
            shift
            ai_continue_mode="custom"
            custom_ai_cmd="$*"
            break
            ;;
    esac
    shift
done

while true; do
    case "$step" in
        1)
            # ── Step 1: Conventional Type Selection via Waymaker Preset (`wm -o awt-type`) ──
            type_output=$($WM_BIN -o awt-type "${MM_TUI_ARGS[@]}")

            # If Esc / canceled in Step 1 -> exit completely back to main awt list
            if [[ -z "$type_output" ]]; then
                exit 0
            fi

            IFS=$'\t' read -r icon prefix <<< "$type_output"
            step=2
            ;;

        2)
            # ── Step 2: Worktree Branch Name via Waymaker Prompt Box (`wm -o awt-prompt`) ──
            initial_val="${prefix}${slug}"
            prompt_str="${icon}"

            branch_input=$($WM_BIN -o awt-prompt prompt="$prompt_str" initial="$initial_val" "${MM_TUI_ARGS[@]}")

            # If user pressed Esc or cancelled -> go back to Step 1
            if [[ -z "$branch_input" ]]; then
                step=1
                continue
            fi

            branch_name=$(echo "$branch_input" | tr ' ' '-' | tr '[:upper:]' '[:lower:]')
            slug="${branch_name#"${prefix}"}"
            step=3
            ;;

        3)
            # ── Step 3: Base Branch Selection via Waymaker Preset (`wm -o awt-base`) ──
            bbase=$($WM_BIN -o awt-base "${MM_TUI_ARGS[@]}" -- "$selected_base")

            # If Esc was pressed in Step 3 -> step back to Step 2 with previous slug preserved!
            if [[ -z "$bbase" ]]; then
                step=2
                continue
            fi

            # Check if AI sessions exist in the origin tmux session
            if [[ "$ai_continue_mode" == "no" ]]; then
                ai_cmd="__FRESH__"
                step=5
            elif [[ "$ai_continue_mode" == "custom" ]]; then
                ai_cmd="$custom_ai_cmd"
                step=5
            elif [[ "$ai_continue_mode" == "yes" ]]; then
                detected_ai=$(awt_ai_detect_all | head -n 1)
                if [ -n "$detected_ai" ]; then
                    ai_cmd=$(echo "$detected_ai" | awk -F'\t' '{print $6}')
                    step=5
                else
                    step=4
                fi
            else
                detected_count=$(awt_ai_detect_all | wc -l)
                if [ "$detected_count" -gt 0 ]; then
                    step=4
                else
                    ai_cmd="__FRESH__"
                    step=5
                fi
            fi
            ;;

        4)
            # ── Step 4: AI Conversation Selection via Waymaker Preset (`wm -o awt-ai`) ──
            ai_output=$($WM_BIN -o awt-ai "${MM_TUI_ARGS[@]}")

            # If Esc was pressed in Step 4 -> step back to Step 3 (base selection)
            if [[ -z "$ai_output" ]]; then
                step=3
                continue
            fi

            ai_cmd="$ai_output"
            step=5
            ;;

        5)
            # ── Step 5: Provision Worktree via Native Git, Run Hooks & Connect via Sesh ──
            repo_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
            branch_folder="${branch_name//\//-}"
            target_dir="$repo_root/../$branch_folder"

            printf "\n\033[1;32m󰄬 Creating worktree '%s' (base: %s)...\033[0m\n" "$branch_name" "$bbase"

            git worktree add "$target_dir" -b "$branch_name" "$bbase" 2>/dev/null || \
            git worktree add "$target_dir" "$branch_name" 2>/dev/null || {
                printf "\n\033[1;31m󰅖 Failed to create worktree at %s\033[0m\n" "$target_dir"
                exit 1
            }
            git -C "$target_dir" config "branch.${branch_name}.base" "$bbase" 2>/dev/null || true
            if git -C "$target_dir" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
                timeout 3s git -C "$target_dir" pull --ff-only 2>/dev/null || \
                timeout 3s git -C "$target_dir" pull --ff-only origin "$branch_name" 2>/dev/null || true
            fi

            # Run post-create hook if present
            if [[ -x "$HOME/.config/waymaker/hooks/post-create.sh" ]]; then
                "$HOME/.config/waymaker/hooks/post-create.sh" "$target_dir" "$branch_name" "$bbase" 2>/dev/null || true
            elif [[ -x "$HOME/.config/matchmaker/hooks/post-create.sh" ]]; then
                "$HOME/.config/matchmaker/hooks/post-create.sh" "$target_dir" "$branch_name" "$bbase" 2>/dev/null || true
            fi

            # Connect via Sesh / Tmux unless --no-tmux / --no-connect was requested
            if [[ $connect_tmux -eq 1 ]]; then
                touch "/tmp/awt_new_created_${USER:-user}" 2>/dev/null || true

                repo_parent=$(basename "$(dirname "$repo_root")")
                if [[ "$repo_parent" == ".dotfiles" ]]; then
                    session_name="_dotfiles/$branch_folder"
                else
                    session_name="${repo_parent}/${branch_folder}"
                fi

                if [ -n "$ai_cmd" ] && [ "$ai_cmd" != "__FRESH__" ]; then
                    if ! tmux has-session -t "$session_name" 2>/dev/null; then
                        tmux new-session -d -s "$session_name" -c "$target_dir"
                        tmux send-keys -t "$session_name:0.0" "$ai_cmd" C-m
                    fi
                    if command -v wm >/dev/null 2>&1; then
                        wm connect "$session_name" 2>/dev/null || tmux switch-client -t "$session_name" 2>/dev/null || true
                    elif command -v sesh >/dev/null 2>&1; then
                        sesh connect "$session_name" 2>/dev/null || tmux switch-client -t "$session_name" 2>/dev/null || true
                    else
                        tmux switch-client -t "$session_name" 2>/dev/null || true
                    fi
                else
                    if command -v wm >/dev/null 2>&1; then
                        wm connect "$target_dir"
                    elif command -v sesh >/dev/null 2>&1; then
                        sesh connect "$target_dir"
                    fi
                fi

                # Dismiss popup modal completely so user lands cleanly in the new session
                if [ -n "$TMUX" ]; then
                    tmux display-popup -C 2>/dev/null || true
                fi
            fi
            exit 0
            ;;
    esac
done
