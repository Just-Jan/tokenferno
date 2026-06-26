pub mod claude;
pub mod copilot;
pub mod fswatch;

use crate::model::UsageEvent;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub type EventSender = mpsc::Sender<IngestMessage>;

/// Intern a source string into one of the known `&'static str` aliases used
/// throughout the aggregator. Unknown values are leaked (rare; would only
/// happen if a third-party process feeds us a custom source name over IPC).
pub fn intern_source(s: &str) -> &'static str {
    match s {
        "claude" => "claude",
        "copilot" => "copilot",
        _ => Box::leak(s.to_string().into_boxed_str()),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IngestMessage {
    Event(UsageEvent),
    FileCount { source: String, count: usize },
    Activity { source: String },
    /// Detected the *start* of an inference request (before usage lands).
    RequestStart { source: String },
    /// Synthetic small "drip" burn (e.g. tool_use marker on Claude) used to
    /// keep the meter alive during long requests.
    MicroBurn { source: String, tokens: u32 },
    /// Streaming partial-token estimate from an in-flight request (e.g. SSE
    /// `delta.content` chunks). Reconciled at completion against the
    /// authoritative `Event`.
    PartialDelta { source: String, est_tokens: u32 },
    Error(String),
}
