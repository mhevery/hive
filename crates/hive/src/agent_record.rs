use chrono::{DateTime, Utc};
use std::path::PathBuf;

/// Status of an agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    /// Agent is actively generating a response or using tools.
    Thinking,
    /// Agent is waiting for user input or the next turn.
    Waiting,
    // Future extensions: Completed, Error, etc. can be added here.
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentStatus::Thinking => write!(f, "Thinking"),
            AgentStatus::Waiting => write!(f, "Waiting"),
        }
    }
}

/// The source/agent type for the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentSource {
    Grok,
    Codex,
}

impl std::fmt::Display for AgentSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentSource::Grok => write!(f, "Grok"),
            AgentSource::Codex => write!(f, "Codex"),
        }
    }
}

/// A record storing data about a single agent session.
/// This is the core uniform representation used by Hive for its dashboard/overview,
/// regardless of the underlying agent (Grok Build, Codex, Claude, Aider, etc.).
#[derive(Debug, Clone)]
pub struct AgentRecord {
    /// Id of the agent (string) — typically the session UUID from the agent's metadata.
    pub id: String,

    /// Summary (string) of what the agent is working on.
    /// This starts as the agent's native summary. The CLI may replace it with
    /// a generated display summary before rendering.
    pub summary: String,

    /// User-side transcript text extracted from the session, when available.
    /// Processors populate this as raw parsed data; higher layers decide whether
    /// to summarize it.
    pub user_text: Option<String>,

    /// Current status of the agent.
    pub status: AgentStatus,

    /// Timestamp of the last generated message (from the agent/assistant side).
    pub last_generated_msg: DateTime<Utc>,

    /// Working directory of where the conversation took place.
    /// This is the key field for knowing "in which directory" the agent was operating.
    pub working_dir: PathBuf,

    /// The agent source (Grok or Codex).
    pub source: AgentSource,
}

impl AgentRecord {
    /// Creates a new AgentRecord with the given fields.
    /// Timestamps should use UTC.
    pub fn new(
        id: impl Into<String>,
        summary: impl Into<String>,
        status: AgentStatus,
        last_generated_msg: DateTime<Utc>,
        working_dir: PathBuf,
        source: AgentSource,
    ) -> Self {
        Self {
            id: id.into(),
            summary: summary.into(),
            user_text: None,
            status,
            last_generated_msg,
            working_dir,
            source,
        }
    }

    pub fn with_user_text(mut self, user_text: Option<String>) -> Self {
        self.user_text = user_text.filter(|text| !text.trim().is_empty());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::path::PathBuf;

    #[test]
    fn test_agent_record_creation_and_display() {
        let record = AgentRecord::new(
            "test-id-123",
            "Refactoring the auth module to use async",
            AgentStatus::Thinking,
            Utc::now(),
            PathBuf::from("/Users/misko/work/some-project"),
            AgentSource::Grok,
        );

        assert_eq!(record.id, "test-id-123");
        assert_eq!(record.summary, "Refactoring the auth module to use async");
        assert_eq!(record.user_text, None);
        assert_eq!(record.status, AgentStatus::Thinking);
        assert_eq!(
            record.working_dir,
            PathBuf::from("/Users/misko/work/some-project")
        );
        assert_eq!(record.status.to_string(), "Thinking");
        assert_eq!(record.source.to_string(), "Grok");
    }
}
