#!/usr/bin/env bash
# awt-ai-list.sh - Formats AI conversations list for Waymaker (`wm -o awt-ai`)

SCRIPT_DIR=$(dirname "$(readlink -f "$0" 2>/dev/null || realpath "$0")")
# shellcheck source=/dev/null
. "$SCRIPT_DIR/awt-ai-detect.sh" 2>/dev/null || true

cur_lines=()
other_lines=()

while IFS=$'\t' read -r is_cur ai_tool conv_id conv_title label resume_cmd; do
    [ -z "$ai_tool" ] && continue

    display_title="$conv_title"
    if [ -z "$display_title" ] || [ "$display_title" = "Active Session" ]; then
        if [ -n "$conv_id" ] && [ "$conv_id" != "none" ]; then
            if [ ${#conv_id} -gt 18 ]; then
                display_title="${conv_id:0:8}..${conv_id: -6}"
            else
                display_title="$conv_id"
            fi
        else
            display_title="Active Session"
        fi
    fi

    # Trim display title if overly long
    if [ ${#display_title} -gt 40 ]; then
        display_title="${display_title:0:37}..."
    fi

    if [[ "$is_cur" -eq 1 ]]; then
        cur_lines+=( "$(printf "\033[1;35m󱐋 %s\033[0m \033[35m(%s)\033[0m\t\033[35mcurrent pane\033[0m\t\033[1;32mcontinue\033[0m\t%s" "$ai_tool" "$display_title" "$resume_cmd")" )
    else
        other_lines+=( "$(printf "\033[1;36m󱐋 %s\033[0m \033[36m(%s)\033[0m\t\033[2m%s\033[0m\t\033[1;32mcontinue\033[0m\t%s" "$ai_tool" "$display_title" "$label" "$resume_cmd")" )
    fi
done < <(awt_ai_detect_all)

# 1. Output Current Pane AI at Row 0
for line in "${cur_lines[@]}"; do
    echo -e "$line"
done

# 2. Output Other AI sessions in the same tmux session
for line in "${other_lines[@]}"; do
    echo -e "$line"
done

# 3. Always include Fresh Conversation option
printf "\033[1;33m󰓹 Fresh Conversation\033[0m\t\033[2mdefault startup\033[0m\t\033[33mstart new\033[0m\t__FRESH__\n"
