#!/usr/bin/env bash
# awt-ai-detect.sh - Extensible AI Conversation Discovery Driver for AWT
# Detects active AI sessions across Tmux panes (agy, opencode, claude, codex, etc.)
# Extracts EXACT conversation/session IDs and titles for each pane.

detect_agy_conv_id() {
    local pid="$1"
    local conv_id=""

    # 1. Inspect open file descriptors for presence lock or conversation db
    conv_id=$(readlink /proc/"$pid"/fd/* 2>/dev/null | grep -E 'antigravity-cli/(conversations|presence)' | sed -E 's/.*(conversations|presence)\/([a-f0-9-]+)(\.db|\.lock).*/\2/' | head -n 1)
    if [ -n "$conv_id" ]; then
        echo "$conv_id"
        return 0
    fi

    # 2. Inspect crash log in fd (contains UUID in filename)
    conv_id=$(readlink /proc/"$pid"/fd/* 2>/dev/null | grep -E 'crashes/crash_' | sed -E 's/.*crash_[0-9]+_([a-f0-9-]+)\.log.*/\1/' | head -n 1)
    if [ -n "$conv_id" ]; then
        echo "$conv_id"
        return 0
    fi

    # 3. Inspect process cmdline
    local cmdline
    cmdline=$(cat /proc/"$pid"/cmdline 2>/dev/null | tr '\0' ' ')
    if [[ "$cmdline" =~ --conversation[[:space:]=]+([a-f0-9-]+) ]]; then
        echo "${BASH_REMATCH[1]}"
        return 0
    fi

    echo ""
}

get_agy_title() {
    local conv_id="$1"
    local title=""
    if [ -n "$conv_id" ] && [ -f "$HOME/.gemini/antigravity-cli/conversation_summaries.db" ] && command -v sqlite3 >/dev/null 2>&1; then
        title=$(sqlite3 "$HOME/.gemini/antigravity-cli/conversation_summaries.db" "SELECT title FROM conversation_summaries WHERE conversation_id = '$conv_id' LIMIT 1" 2>/dev/null)
    fi
    echo "$title"
}

detect_opencode_conv_id() {
    local pid="$1"
    local cmdline
    cmdline=$(cat /proc/"$pid"/cmdline 2>/dev/null | tr '\0' ' ')
    local conv_id=""
    if [[ "$cmdline" =~ -s[[:space:]]+([^[:space:]]+) ]] || [[ "$cmdline" =~ --session[[:space:]]+([^[:space:]]+) ]]; then
        conv_id="${BASH_REMATCH[1]}"
    fi
    if [ -z "$conv_id" ] && command -v opencode >/dev/null 2>&1; then
        conv_id=$(opencode session list --format json 2>/dev/null | python3 -c "import sys,json; data=json.load(sys.stdin); print(data[0]['id'] if data else '')" 2>/dev/null || true)
    fi
    echo "$conv_id"
}

get_opencode_title() {
    local conv_id="$1"
    local title=""
    if [ -n "$conv_id" ] && command -v opencode >/dev/null 2>&1; then
        title=$(opencode session list --format json 2>/dev/null | python3 -c "import sys,json; data=json.load(sys.stdin); print(next((x.get('title','') for x in data if x.get('id')=='$conv_id'), ''))" 2>/dev/null || true)
    fi
    echo "$title"
}

detect_claude_conv_id() {
    local pid="$1"
    local cmdline
    cmdline=$(cat /proc/"$pid"/cmdline 2>/dev/null | tr '\0' ' ')
    local conv_id=""
    if [[ "$cmdline" =~ --resume[[:space:]]+([^[:space:]]+) ]]; then
        conv_id="${BASH_REMATCH[1]}"
    fi
    echo "$conv_id"
}

detect_devin_conv_id() {
    local pid="$1"
    local cmdline
    cmdline=$(cat /proc/"$pid"/cmdline 2>/dev/null | tr '\0' ' ')
    local conv_id=""
    if [[ "$cmdline" =~ (-r|--resume)[[:space:]]+([^-[:space:]][^[:space:]]*) ]]; then
        conv_id="${BASH_REMATCH[2]}"
    fi
    if [ -z "$conv_id" ] && command -v devin >/dev/null 2>&1; then
        conv_id=$(devin list --format json 2>/dev/null | jq -r '.[0].id // .[0].session_id // empty' 2>/dev/null || true)
    fi
    echo "$conv_id"
}

get_devin_title() {
    local conv_id="$1"
    local title=""
    if [ -n "$conv_id" ] && command -v devin >/dev/null 2>&1; then
        title=$(devin list --format json 2>/dev/null | jq -r --arg id "$conv_id" '.[] | select((.id // .session_id) == $id) | .title // empty' 2>/dev/null | head -n 1 || true)
    fi
    echo "$title"
}

find_descendants() {
    local pid="$1"
    local children
    children=$(pgrep -P "$pid" 2>/dev/null)
    for c in $children; do
        echo "$c"
        find_descendants "$c"
    done
}

# Returns list of AI sessions:
# Format: is_cur_pane \t ai_name \t conv_id \t conv_title \t win_pane_label \t resume_cmd
awt_ai_detect_all() {
    local current_pane="${AWT_ORIGIN_PANE:-$(tmux display-message -p '#{pane_id}' 2>/dev/null || echo '')}"
    local current_session=$(tmux display-message -p '#{session_name}' 2>/dev/null || echo "")

    [ -z "$current_session" ] && return 0

    local seen_pids=()

    while read -r pane_id win_idx pane_idx pane_pid win_name pane_title; do
        local is_cur=0
        [[ "$pane_id" == "$current_pane" ]] && is_cur=1

        local all_pids=("$pane_pid" $(find_descendants "$pane_pid"))

        for p in "${all_pids[@]}"; do
            [[ " ${seen_pids[*]} " =~ " $p " ]] && continue
            seen_pids+=("$p")

            local comm
            comm=$(cat /proc/"$p"/comm 2>/dev/null || true)
            local cmdline
            cmdline=$(cat /proc/"$p"/cmdline 2>/dev/null | tr '\0' ' ' || true)

            # Skip common shell wrappers to find the actual AI binary
            [[ "$comm" == "bash" || "$comm" == "zsh" || "$comm" == "sh" ]] && continue

            local ai_tool=""
            local conv_id=""
            local conv_title=""
            local resume_cmd=""

            # Driver 1: Antigravity (agy)
            if [[ "$comm" == "agy-bin" || "$comm" == "agy" || "$comm" == "antigravity" ]]; then
                if [[ "$cmdline" != *"grep"* && "$cmdline" != *"awt-ai"* ]]; then
                    conv_id=$(detect_agy_conv_id "$p")
                    ai_tool="agy"
                    if [ -n "$conv_id" ]; then
                        conv_title=$(get_agy_title "$conv_id")
                        resume_cmd="agy --conversation $conv_id"
                    else
                        resume_cmd="agy"
                    fi
                fi
            # Driver 2: OpenCode
            elif [[ "$comm" == "opencode" ]]; then
                if [[ "$cmdline" != *"grep"* && "$cmdline" != *"awt-ai"* ]]; then
                    conv_id=$(detect_opencode_conv_id "$p")
                    ai_tool="opencode"
                    if [ -n "$conv_id" ]; then
                        conv_title=$(get_opencode_title "$conv_id")
                        resume_cmd="opencode -s $conv_id"
                    else
                        resume_cmd="opencode"
                    fi
                fi
            # Driver 3: Claude Code
            elif [[ "$comm" == "claude" ]]; then
                if [[ "$cmdline" != *"grep"* && "$cmdline" != *"awt-ai"* ]]; then
                    conv_id=$(detect_claude_conv_id "$p")
                    ai_tool="claude"
                    if [ -n "$conv_id" ]; then
                        resume_cmd="claude --resume $conv_id"
                    else
                        resume_cmd="claude"
                    fi
                fi
            # Driver 4: Devin CLI
            elif [[ "$comm" == "devin" ]]; then
                if [[ "$cmdline" != *"grep"* && "$cmdline" != *"awt-ai"* ]]; then
                    conv_id=$(detect_devin_conv_id "$p")
                    ai_tool="devin"
                    if [ -n "$conv_id" ]; then
                        conv_title=$(get_devin_title "$conv_id")
                        resume_cmd="devin --resume $conv_id"
                    else
                        resume_cmd="devin --continue"
                    fi
                fi
            fi

            if [ -n "$ai_tool" ]; then
                local label="win ${win_idx}.${pane_idx}"
                [ -n "$win_name" ] && label+=" (${win_name})"
                [ -z "$conv_id" ] && conv_id="none"
                [ -z "$conv_title" ] && conv_title="Active Session"
                [ -z "$resume_cmd" ] && resume_cmd="$ai_tool"
                printf "%s\t%s\t%s\t%s\t%s\t%s\n" "$is_cur" "$ai_tool" "$conv_id" "$conv_title" "$label" "$resume_cmd"
                break
            fi
        done
    done < <(tmux list-panes -s -F "#{pane_id} #{window_index} #{pane_index} #{pane_pid} #{window_name} #{pane_title}" 2>/dev/null)
}

awt_prepare_cross_worktree_resume() {
    local src_wt="$1" dst_wt="$2" resume_cmd="$3"
    if [[ "$resume_cmd" =~ ^claude[[:space:]]+--resume[[:space:]]+([^[:space:]]+) ]]; then
        local sid="${BASH_REMATCH[1]}"
        local src_enc="${src_wt//\//-}"
        local dst_enc="${dst_wt//\//-}"
        local cdir="$HOME/.claude/projects"
        if [[ -f "$cdir/$src_enc/${sid}.jsonl" && ! -f "$cdir/$dst_enc/${sid}.jsonl" ]]; then
            mkdir -p "$cdir/$dst_enc"
            cp -a "$cdir/$src_enc/${sid}.jsonl" "$cdir/$dst_enc/${sid}.jsonl" 2>/dev/null || true
            [[ -d "$cdir/$src_enc/${sid}" ]] && cp -a "$cdir/$src_enc/${sid}" "$cdir/$dst_enc/" 2>/dev/null || true
        fi
    fi
    printf '%s' "$resume_cmd"
}

# If executed directly:
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    awt_ai_detect_all
fi
