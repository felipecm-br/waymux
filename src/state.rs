#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Working,
    AwaitingInput,
    Error,
}

impl From<&str> for AgentState {
    fn from(s: &str) -> Self {
        match s {
            "working" => AgentState::Working,
            "awaiting_input" => AgentState::AwaitingInput,
            "error" => AgentState::Error,
            _ => AgentState::Idle,
        }
    }
}
