//! proptest strategies shared by the property tests: arbitrary JSON shaped like what agents,
//! tmux and `gh` send, so random cases reach the interesting branches instead of only
//! failing on the first missing key.

use proptest::prelude::*;
use serde_json::Value;

/// Keys the hooks and RPC handlers look up, plus some they do not.
const KEYS: &[&str] = &[
    "jsonrpc",
    "method",
    "params",
    "id",
    "pane_id",
    "target",
    "state",
    "timestamp",
    "tool_name",
    "tool_input",
    "toolCall",
    "tool",
    "name",
    "args",
    "input",
    "command",
    "file_path",
    "path",
    "questions",
    "question",
    "url",
    "pattern",
    "title",
    "prompt",
    "transcript_path",
    "transcriptPath",
    "source",
    "hook_event_name",
    "message",
    "number",
    "statusCheckRollup",
    "conclusion",
    "status",
    "commit_state",
    "x",
];

/// Strings that stress parsers: empty, whitespace, slashes, quotes, multibyte and control chars.
pub fn arb_text() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        Just("   \t\n".to_string()),
        Just("/plan   do it".to_string()),
        Just("<USER_REQUEST>x</USER_REQUEST>".to_string()),
        "[a-z_/%@.]{0,12}",
        any::<String>(),
        "\\PC{0,40}",
    ]
}

pub fn arb_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        any::<f64>().prop_map(Value::from),
        arb_text().prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 48, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..5).prop_map(Value::Array),
            prop::collection::vec((prop::sample::select(KEYS), inner), 0..6).prop_map(|kv| {
                Value::Object(kv.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
            }),
        ]
    })
}
