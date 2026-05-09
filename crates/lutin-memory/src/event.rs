use serde::{Deserialize, Serialize};

pub type EventId = i64;
pub type ChatId = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    UserMessage,
    AgentMessage,
    Transcription,
    ToolCall,
    ToolResult,
    Note,
}

impl EventType {
    pub fn as_str(self) -> &'static str {
        match self {
            EventType::UserMessage => "user_message",
            EventType::AgentMessage => "agent_message",
            EventType::Transcription => "transcription",
            EventType::ToolCall => "tool_call",
            EventType::ToolResult => "tool_result",
            EventType::Note => "note",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "user_message" => EventType::UserMessage,
            "agent_message" => EventType::AgentMessage,
            "transcription" => EventType::Transcription,
            "tool_call" => EventType::ToolCall,
            "tool_result" => EventType::ToolResult,
            "note" => EventType::Note,
            _ => return None,
        })
    }

    pub fn is_chat_message(self) -> bool {
        matches!(self, EventType::UserMessage | EventType::AgentMessage)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Ready,
    Failed,
    Stale,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Ready => "ready",
            Status::Failed => "failed",
            Status::Stale => "stale",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Status::Pending,
            "ready" => Status::Ready,
            "failed" => Status::Failed,
            "stale" => Status::Stale,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewEvent {
    pub timestamp: i64,
    pub event_type: EventType,
    pub source: Option<String>,
    pub content: String,
    pub chat_external_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub timestamp: i64,
    pub event_type: EventType,
    pub source: Option<String>,
    pub content: String,
    pub summary: Option<String>,
    pub status: Status,
    pub chat_id: Option<ChatId>,
    pub topics: Vec<String>,
    pub entities: Vec<EntityRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityRef {
    pub name: String,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventMeta {
    pub summary: String,
    pub topics: Vec<String>,
    pub entities: Vec<EntityRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSummary {
    pub title: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitySummary {
    pub summary: String,
}
