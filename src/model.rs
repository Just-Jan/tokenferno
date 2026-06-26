use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Provider {
    Claude,
    Copilot,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Copilot => "copilot",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEvent {
    pub ts: DateTime<Utc>,
    pub provider: Provider,
    pub model: String,
    pub session_id: String,
    pub cwd: Option<PathBuf>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub raw_source: String,
    /// Provider-assigned unique id for this usage record (Copilot completion
    /// `id`, Claude assistant `message.id`). Used to de-duplicate so the same
    /// completion isn't counted twice across persisted-replay + log-backfill
    /// (the main cause of an inflated "tokens burned today"). `None` if the
    /// log didn't carry one.
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Totals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub reasoning: u64,
    pub total: u64,
    pub events: u64,
}

impl Totals {
    pub fn add(&mut self, ev: &UsageEvent) {
        self.input += ev.input_tokens;
        self.output += ev.output_tokens;
        self.cache_read += ev.cache_read_tokens;
        self.cache_creation += ev.cache_creation_tokens;
        self.reasoning += ev.reasoning_tokens;
        self.total += ev.total_tokens;
        self.events += 1;
    }
}

#[derive(Debug, Clone)]
pub struct SessionStat {
    pub provider: Provider,
    pub session_id: String,
    pub model: String,
    pub totals: Totals,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct RecentEvent {
    pub ts: DateTime<Utc>,
    pub provider: Provider,
    pub session_id: String,
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
}

/// One bucket in the rolling sparkline (per minute).
#[derive(Debug, Clone, Copy, Default)]
pub struct BurnBucket {
    pub minute: i64, // unix-minute
    pub tokens: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub generated_at: DateTime<Utc>,
    pub today: Totals,
    pub by_provider: BTreeMap<&'static str, Totals>,
    pub sessions: Vec<SessionStat>,
    pub recent: VecDeque<RecentEvent>,
    pub burn_per_min: Vec<BurnBucket>, // last 30 buckets
    pub burn_per_min_by_provider: BTreeMap<&'static str, Vec<BurnBucket>>,
    pub burn_per_sec: Vec<u64>, // last 60 seconds, oldest → newest
    pub burn_per_sec_by_provider: BTreeMap<&'static str, Vec<u64>>,
    /// Last 600 × 100 ms buckets (60 s window, oldest → newest).
    pub burn_per_decisec: Vec<u64>,
    pub burn_per_decisec_by_provider: BTreeMap<&'static str, Vec<u64>>,
    pub current_rate_tpm: f64, // tokens / minute, rolling 5 min
    pub current_rate_tpm_by_provider: BTreeMap<&'static str, f64>,
    pub current_rate_tps: f64, // tokens / second, rolling 10 s
    pub current_rate_tps_by_provider: BTreeMap<&'static str, f64>,
    pub instant_rate_tps: f64, // tokens / second, rolling 500 ms
    pub instant_rate_tps_by_provider: BTreeMap<&'static str, f64>,
    /// Tokens / second over the last 200 ms — drives the big-number / needle.
    pub micro_rate_tps: f64,
    pub micro_rate_tps_by_provider: BTreeMap<&'static str, f64>,
    /// Drip burn rate (events with total_tokens ≤ 100) per minute, last 5 min.
    pub drip_per_min: f64,
    pub peak_rate_tpm: f64,
    pub peak_rate_at: Option<DateTime<Utc>>,
    pub last_event_at: Option<DateTime<Utc>>,
    pub last_event_tokens: u64,
    pub events_per_min: f64,
    pub watched_files: usize,
    pub dropped_events: u64,
    pub last_activity_at: BTreeMap<&'static str, DateTime<Utc>>,
    pub in_flight: BTreeMap<&'static str, u32>,
    pub oldest_in_flight_at: BTreeMap<&'static str, DateTime<Utc>>,
    /// Per-provider in-flight: list of (n, oldest_start) for compact UI.
    pub in_flight_by_provider: BTreeMap<&'static str, (u32, DateTime<Utc>)>,
    /// Median tok/sec observed historically per provider — used to project
    /// "EST" tokens during in-flight requests until the real usage lands.
    pub median_tps_by_provider: BTreeMap<&'static str, f64>,
    /// Median total response duration (secs) per provider — used for the
    /// fun-mode progress bar to know "how full should this request feel".
    pub median_response_secs_by_provider: BTreeMap<&'static str, f64>,
    /// Time-to-first-activity (ms) per provider: last + median over a small
    /// rolling buffer.
    pub ttf_last_ms_by_provider: BTreeMap<&'static str, u64>,
    pub ttf_median_ms_by_provider: BTreeMap<&'static str, u64>,
    pub last_error: Option<String>,
}
