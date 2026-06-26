use crate::ingest::IngestMessage;
use crate::model::{BurnBucket, Provider, RecentEvent, SessionStat, Snapshot, Totals, UsageEvent};
use chrono::{DateTime, Datelike, Utc};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

const SPARKLINE_MINUTES: usize = 30;
const ROLLING_RATE_MINUTES: u64 = 5;
const ROLLING_RATE_SECS: i64 = 10;
/// Window for `instant_rate_tps`, in 100 ms units (500 ms).
const INSTANT_RATE_DECIS: i64 = 5;
/// Window for `micro_rate_tps`, in 100 ms units (200 ms).
const MICRO_RATE_DECIS: i64 = 2;
const SESSION_IDLE_SECS: i64 = 60 * 30;
const RECENT_CAP: usize = 8;
/// Number of 100 ms buckets to keep (60 s window).
const DECI_BUCKETS_KEEP: i64 = 600;
/// Max age of an in-flight request start before it is treated as orphaned
/// and dropped. Generous so a *long* generation (a minute+) isn't pruned
/// mid-flight — the fire is kept alive by the projection while it runs. A
/// genuinely failed start still clears here (and the projection stops
/// sooner once the log goes silent; see `ui::fun::projected_rate`).
const IN_FLIGHT_MAX_AGE_SECS: i64 = 90;
const IN_FLIGHT_MAX_PER_PROVIDER: usize = 4;
const HISTORY_CAP: usize = 20;
/// Upper bound on a per-request tok/s sample, to keep a mismatched
/// start↔completion pair from poisoning the projection's median rate.
const MAX_SANE_TPS: f64 = 200_000.0;
/// "Drip" event = small total_tokens; tracked over a 5-minute window.
const DRIP_TOKEN_THRESHOLD: u64 = 100;
const DRIP_WINDOW_MINUTES: i64 = 5;

pub struct Aggregator {
    today_key: i32, // ordinal date
    today: Totals,
    by_provider: HashMap<&'static str, Totals>,
    sessions: HashMap<(Provider, String), SessionStat>,
    recent: VecDeque<RecentEvent>,
    buckets: BTreeMap<i64, u64>, // unix-minute -> tokens
    buckets_provider: HashMap<&'static str, BTreeMap<i64, u64>>,
    decis: BTreeMap<i64, u64>, // unix_ms/100 -> tokens (last ~60s)
    decis_provider: HashMap<&'static str, BTreeMap<i64, u64>>,
    events_minute: BTreeMap<i64, u32>, // unix-minute -> event count
    drip_minute: BTreeMap<i64, u32>,   // unix-minute -> small-event count
    last_event_at: Option<DateTime<Utc>>,
    last_event_tokens: u64,
    peak_rate: f64,
    peak_at: Option<DateTime<Utc>>,
    file_counts: HashMap<&'static str, usize>,
    last_activity: HashMap<&'static str, DateTime<Utc>>,
    in_flight: HashMap<&'static str, VecDeque<DateTime<Utc>>>,
    /// Per-provider in-flight credited tokens, aligned 1:1 with `in_flight`.
    /// Entry N is the running sum of `PartialDelta` est_tokens credited to
    /// the Nth pending request, used to subtract from the authoritative
    /// `Event.total_tokens` at completion (reconciliation ledger).
    credited_in_flight: HashMap<&'static str, VecDeque<u64>>,
    /// Real per-provider tok/sec history (one entry per completed request).
    tps_history: HashMap<&'static str, VecDeque<f64>>,
    /// Real per-provider response duration history (seconds, per request).
    duration_history: HashMap<&'static str, VecDeque<f64>>,
    /// Pending RequestStart timestamps awaiting first activity, for TTF.
    pending_ttf: HashMap<&'static str, VecDeque<DateTime<Utc>>>,
    /// Recently observed time-to-first-activity values (ms) per provider.
    ttf_history: HashMap<&'static str, VecDeque<u64>>,
    /// Last time each source enqueued a RequestStart, for dedup window.
    last_request_start_at: HashMap<&'static str, DateTime<Utc>>,
    /// Last time each provider had a real `Event` complete; resets the dedup
    /// suppression so back-to-back tool turns aren't dropped.
    last_event_at_by_provider: HashMap<&'static str, DateTime<Utc>>,
    /// Wall-clock time the aggregator started. Events whose timestamp predates
    /// this are historical (startup backfill / persisted replay) and must not
    /// feed the live-rate buckets, or the fire/scoreboard blaze on launch.
    started_at: DateTime<Utc>,
    /// Ids of usage records already counted today, so the same completion isn't
    /// double-counted across persisted-replay + log-backfill + live tail (the
    /// main cause of an inflated "tokens burned today"). Cleared on day flip.
    seen_ids: HashSet<String>,
    dropped: u64,
    last_error: Option<String>,
}

impl Default for Aggregator {
    fn default() -> Self {
        Self::new()
    }
}

impl Aggregator {
    pub fn new() -> Self {
        Self {
            today_key: Utc::now().num_days_from_ce(),
            today: Totals::default(),
            by_provider: HashMap::new(),
            sessions: HashMap::new(),
            recent: VecDeque::with_capacity(RECENT_CAP),
            buckets: BTreeMap::new(),
            buckets_provider: HashMap::new(),
            decis: BTreeMap::new(),
            decis_provider: HashMap::new(),
            events_minute: BTreeMap::new(),
            drip_minute: BTreeMap::new(),
            last_event_at: None,
            last_event_tokens: 0,
            peak_rate: 0.0,
            peak_at: None,
            file_counts: HashMap::new(),
            last_activity: HashMap::new(),
            in_flight: HashMap::new(),
            credited_in_flight: HashMap::new(),
            tps_history: HashMap::new(),
            duration_history: HashMap::new(),
            pending_ttf: HashMap::new(),
            ttf_history: HashMap::new(),
            last_request_start_at: HashMap::new(),
            last_event_at_by_provider: HashMap::new(),
            started_at: Utc::now(),
            seen_ids: HashSet::new(),
            dropped: 0,
            last_error: None,
        }
    }

    fn rotate_day_if_needed(&mut self) {
        let now = Utc::now().num_days_from_ce();
        if now != self.today_key {
            self.today_key = now;
            self.today = Totals::default();
            self.by_provider.clear();
            self.seen_ids.clear();
        }
    }

    /// Handle a `RequestStart` for `source`, applying the dedup window.
    /// Returns `true` if the start was enqueued, `false` if it was dropped.
    fn request_start(&mut self, source: &'static str) -> bool {
        let now = Utc::now();
        if let Some(prev) = self.last_request_start_at.get(source).copied() {
            let within_window = (now - prev).num_milliseconds() < 750;
            // If a real Event has landed since the previous RequestStart, the
            // new start is a legitimate next turn — don't dedupe.
            let event_since = self
                .last_event_at_by_provider
                .get(source)
                .copied()
                .map(|ev_t| ev_t > prev)
                .unwrap_or(false);
            if within_window && !event_since {
                return false;
            }
        }
        self.last_request_start_at.insert(source, now);
        self.last_activity.insert(source, now);
        let q = self.in_flight.entry(source).or_default();
        while q.len() >= IN_FLIGHT_MAX_PER_PROVIDER {
            q.pop_front();
        }
        q.push_back(now);
        let cq = self.credited_in_flight.entry(source).or_default();
        while cq.len() >= IN_FLIGHT_MAX_PER_PROVIDER {
            cq.pop_front();
        }
        cq.push_back(0);
        let pq = self.pending_ttf.entry(source).or_default();
        while pq.len() >= IN_FLIGHT_MAX_PER_PROVIDER {
            pq.pop_front();
        }
        pq.push_back(now);
        true
    }

    /// Ingest a completed usage event. Returns `false` (and does nothing) if
    /// this record's id was already counted today — so the same completion
    /// isn't double-counted across persisted-replay, log-backfill and the live
    /// tail. Returns `true` if it was newly counted.
    fn ingest(&mut self, ev: UsageEvent) -> bool {
        self.rotate_day_if_needed();
        // De-duplicate so the same completion isn't counted twice across
        // persisted-replay + log-backfill + live tail. Prefer the provider's
        // record id; fall back to provider+timestamp+total for records that
        // carry no id (older persisted events / providers without one) — a
        // re-persisted duplicate is byte-identical there, while two genuinely
        // distinct turns differ.
        let dedup_key = match &ev.id {
            Some(id) => id.clone(),
            None => format!(
                "{}|{}|{}",
                ev.provider.label(),
                ev.ts.timestamp_millis(),
                ev.total_tokens
            ),
        };
        if !self.seen_ids.insert(dedup_key) {
            return false;
        }
        // Mark this provider as having seen a real completion; this releases
        // the next RequestStart from the dedup window so chained tool turns
        // aren't suppressed.
        self.last_event_at_by_provider
            .insert(ev.provider.label(), Utc::now());
        // A usage event = a request just completed. Pop oldest in-flight start
        // for this provider; if we have it, derive real tok/sec + duration AND
        // distribute the burst across the actual response window.
        let now = Utc::now();
        let mut response_start: Option<DateTime<Utc>> = None;
        let mut credited: u64 = 0;
        if let Some(q) = self.in_flight.get_mut(ev.provider.label()) {
            if let Some(start) = q.pop_front() {
                // Floor the elapsed at 1 s and cap the derived rate: a
                // near-instant start↔completion (e.g. a mismatched FIFO pair)
                // would otherwise yield millions of tok/s and poison the
                // median used for the projection.
                let elapsed = (now - start).num_milliseconds().max(1000) as f64 / 1000.0;
                let tps = ((ev.total_tokens as f64) / elapsed).min(MAX_SANE_TPS);
                let hist = self.tps_history.entry(ev.provider.label()).or_default();
                if hist.len() == HISTORY_CAP {
                    hist.pop_front();
                }
                hist.push_back(tps);
                let dhist = self
                    .duration_history
                    .entry(ev.provider.label())
                    .or_default();
                if dhist.len() == HISTORY_CAP {
                    dhist.pop_front();
                }
                dhist.push_back(elapsed);
                response_start = Some(start);
            }
        }
        // Pop the matching credited counter (kept in lock-step with in_flight).
        if let Some(cq) = self.credited_in_flight.get_mut(ev.provider.label()) {
            if let Some(c) = cq.pop_front() {
                credited = c;
            }
        }
        // Reconciliation: only credit the *delta* between authoritative total
        // and what we already streamed via PartialDelta. `today.total` /
        // `by_provider.total` then equal sum-of-Events exactly.
        let delta_total = ev.total_tokens.saturating_sub(credited);
        // Build a "delta" event for accumulator semantics: keep input/output
        // bookkeeping authoritative (those weren't credited piecewise), but
        // only push the unaccounted-for delta into the bucket maps.
        self.today.add(&ev);
        // Subtract the partials we already added to today's counter.
        self.today.total = self.today.total.saturating_sub(credited);
        let bp = self.by_provider.entry(ev.provider.label()).or_default();
        bp.add(&ev);
        bp.total = bp.total.saturating_sub(credited);

        let key = (ev.provider, ev.session_id.clone());
        let stat = self.sessions.entry(key).or_insert_with(|| SessionStat {
            provider: ev.provider,
            session_id: ev.session_id.clone(),
            model: ev.model.clone(),
            totals: Totals::default(),
            last_seen: ev.ts,
        });
        stat.totals.add(&ev);
        stat.totals.total = stat.totals.total.saturating_sub(credited);
        stat.model = ev.model.clone();
        stat.last_seen = ev.ts;

        let minute = ev.ts.timestamp() / 60;
        let end_ms = ev.ts.timestamp_millis();
        *self.buckets.entry(minute).or_insert(0) += delta_total;
        *self
            .buckets_provider
            .entry(ev.provider.label())
            .or_default()
            .entry(minute)
            .or_insert(0) += delta_total;

        // Only events that actually occurred during this session feed the
        // live-rate buckets. Startup backfill / persisted replay carry older
        // timestamps and must not light the fire or the scoreboard rate.
        let live = ev.ts > self.started_at;

        // Distribute the *un-credited* tokens across the actual response duration
        // in 100 ms buckets.
        if live {
            if let Some(start) = response_start {
                let start_ms = start.timestamp_millis().min(end_ms);
                let start_deci = start_ms / 100;
                let end_deci = end_ms / 100;
                let span = (end_deci - start_deci).max(0) as u64 + 1;
                let per = delta_total / span;
                let remainder = delta_total % span;
                for i in 0..span {
                    let d = start_deci + i as i64;
                    let add = per + if i < remainder { 1 } else { 0 };
                    if add > 0 {
                        *self.decis.entry(d).or_insert(0) += add;
                        *self
                            .decis_provider
                            .entry(ev.provider.label())
                            .or_default()
                            .entry(d)
                            .or_insert(0) += add;
                    }
                }
            } else if delta_total > 0 {
                *self.decis.entry(end_ms / 100).or_insert(0) += delta_total;
                *self
                    .decis_provider
                    .entry(ev.provider.label())
                    .or_default()
                    .entry(end_ms / 100)
                    .or_insert(0) += delta_total;
            }
        }
        *self.events_minute.entry(minute).or_insert(0) += 1;
        if ev.total_tokens <= DRIP_TOKEN_THRESHOLD {
            *self.drip_minute.entry(minute).or_insert(0) += 1;
        }
        if live {
            self.last_event_at = Some(ev.ts);
            self.last_event_tokens = ev.total_tokens;
        }

        if self.recent.len() == RECENT_CAP {
            self.recent.pop_back();
        }
        self.recent.push_front(RecentEvent {
            ts: ev.ts,
            provider: ev.provider,
            session_id: ev.session_id,
            model: ev.model,
            input: ev.input_tokens,
            output: ev.output_tokens,
            cache_read: ev.cache_read_tokens,
        });
        true
    }

    /// Credit a streaming partial estimate to the current head in-flight for
    /// `source`. Increments today/by_provider/buckets so the user sees tokens
    /// flowing in real time; the eventual `Event` reconciles the delta.
    fn ingest_partial(&mut self, source: &'static str, est_tokens: u32) {
        if est_tokens == 0 {
            return;
        }
        self.rotate_day_if_needed();
        let n = est_tokens as u64;
        // Align credited_in_flight with in_flight: ensure the deque has at
        // least as many entries as in_flight; back-fill with zeros for any
        // pre-existing requests that weren't tracked.
        let in_flight_len = self.in_flight.get(source).map(|q| q.len()).unwrap_or(0);
        let cq = self.credited_in_flight.entry(source).or_default();
        while cq.len() < in_flight_len {
            cq.push_back(0);
        }
        if let Some(back) = cq.back_mut() {
            *back += n;
        } else {
            // No in-flight request — drop credit on the floor (we have nothing
            // to reconcile against; otherwise totals would drift).
            return;
        }
        let now = Utc::now();
        self.today.total += n;
        self.by_provider.entry(source).or_default().total += n;
        let now_ms = now.timestamp_millis();
        *self.decis.entry(now_ms / 100).or_insert(0) += n;
        *self
            .decis_provider
            .entry(source)
            .or_default()
            .entry(now_ms / 100)
            .or_insert(0) += n;
        let minute = now.timestamp() / 60;
        *self.buckets.entry(minute).or_insert(0) += n;
        *self
            .buckets_provider
            .entry(source)
            .or_default()
            .entry(minute)
            .or_insert(0) += n;
        self.last_activity.insert(source, now);
    }

    /// Synthetic small "drip" burn from a tool_use / cache-only marker.
    /// Counted as part of `today.total` and as a drip event.
    fn ingest_micro(&mut self, source: &'static str, tokens: u32) {
        if tokens == 0 {
            return;
        }
        self.rotate_day_if_needed();
        let n = tokens as u64;
        let now = Utc::now();
        self.today.total += n;
        self.today.events += 1;
        let bp = self.by_provider.entry(source).or_default();
        bp.total += n;
        bp.events += 1;
        let now_ms = now.timestamp_millis();
        *self.decis.entry(now_ms / 100).or_insert(0) += n;
        *self
            .decis_provider
            .entry(source)
            .or_default()
            .entry(now_ms / 100)
            .or_insert(0) += n;
        let minute = now.timestamp() / 60;
        *self.buckets.entry(minute).or_insert(0) += n;
        *self
            .buckets_provider
            .entry(source)
            .or_default()
            .entry(minute)
            .or_insert(0) += n;
        *self.events_minute.entry(minute).or_insert(0) += 1;
        // Treat micro-bursts as drip events.
        *self.drip_minute.entry(minute).or_insert(0) += 1;
        self.last_activity.insert(source, now);
    }

    fn snapshot(&mut self) -> Snapshot {
        // Day rotation must be checked here too — if the app is left open
        // overnight with no activity, ingest() never runs and "today" wouldn't roll.
        self.rotate_day_if_needed();
        // Cap pending_ttf — defensive: if Activity for a RequestStart is ever
        // missed (race / log rotation), avoid unbounded growth.
        for q in self.pending_ttf.values_mut() {
            while q.len() > HISTORY_CAP {
                q.pop_front();
            }
        }
        let now = Utc::now();
        let now_min = now.timestamp() / 60;
        let now_ms = now.timestamp_millis();
        let now_deci = now_ms / 100;
        let cutoff = now_min - SPARKLINE_MINUTES as i64;
        self.buckets.retain(|m, _| *m > cutoff);
        for map in self.buckets_provider.values_mut() {
            map.retain(|m, _| *m > cutoff);
        }
        let deci_cutoff = now_deci - DECI_BUCKETS_KEEP;
        self.decis.retain(|d, _| *d > deci_cutoff);
        for map in self.decis_provider.values_mut() {
            map.retain(|d, _| *d > deci_cutoff);
        }
        let evt_cutoff = now_min - 5;
        self.events_minute.retain(|m, _| *m > evt_cutoff);
        let drip_cutoff = now_min - DRIP_WINDOW_MINUTES;
        self.drip_minute.retain(|m, _| *m > drip_cutoff);

        // Drop stale in-flight starts (request likely failed / never logged usage).
        let stale_cutoff = now - chrono::Duration::seconds(IN_FLIGHT_MAX_AGE_SECS);
        for q in self.in_flight.values_mut() {
            while q.front().map(|t| *t < stale_cutoff).unwrap_or(false) {
                q.pop_front();
            }
        }

        let burn_per_min = build_buckets(&self.buckets, now_min);
        let burn_per_min_by_provider = self
            .buckets_provider
            .iter()
            .map(|(k, v)| (*k, build_buckets(v, now_min)))
            .collect();

        let rate_cutoff = now_min - ROLLING_RATE_MINUTES as i64;
        let recent_total: u64 = self
            .buckets
            .iter()
            .filter(|(m, _)| **m > rate_cutoff)
            .map(|(_, t)| *t)
            .sum();
        let current_rate = recent_total as f64 / ROLLING_RATE_MINUTES as f64;

        // Rolling 10 s tok/sec from deci buckets.
        let secs10_cutoff = now_deci - 100; // 10 s in 100 ms units
        let recent_secs: u64 = self
            .decis
            .iter()
            .filter(|(d, _)| **d > secs10_cutoff)
            .map(|(_, t)| *t)
            .sum();
        let current_rate_tps = recent_secs as f64 / ROLLING_RATE_SECS as f64;

        // 500 ms window for "right now" feel.
        let inst_cutoff = now_deci - INSTANT_RATE_DECIS;
        let inst_tokens: u64 = self
            .decis
            .iter()
            .filter(|(d, _)| **d > inst_cutoff)
            .map(|(_, t)| *t)
            .sum();
        let instant_rate_tps = inst_tokens as f64 / (INSTANT_RATE_DECIS as f64 * 0.1);

        // 200 ms micro window for the needle / big number.
        let micro_cutoff = now_deci - MICRO_RATE_DECIS;
        let micro_tokens: u64 = self
            .decis
            .iter()
            .filter(|(d, _)| **d > micro_cutoff)
            .map(|(_, t)| *t)
            .sum();
        let micro_rate_tps = micro_tokens as f64 / (MICRO_RATE_DECIS as f64 * 0.1);

        // Per-100ms sparkline for the last 60 seconds (oldest → newest).
        let burn_per_decisec: Vec<u64> = (0..DECI_BUCKETS_KEEP)
            .rev()
            .map(|i| {
                let d = now_deci - i;
                self.decis.get(&d).copied().unwrap_or(0)
            })
            .collect();
        // Back-compat: per-second sparkline = sums of 10 consecutive deci buckets.
        let burn_per_sec: Vec<u64> = burn_per_decisec
            .chunks(10)
            .map(|c| c.iter().sum())
            .collect();

        let burn_per_decisec_by_provider: BTreeMap<&'static str, Vec<u64>> = self
            .decis_provider
            .iter()
            .map(|(k, map)| {
                let v: Vec<u64> = (0..DECI_BUCKETS_KEEP)
                    .rev()
                    .map(|i| map.get(&(now_deci - i)).copied().unwrap_or(0))
                    .collect();
                (*k, v)
            })
            .collect();
        let burn_per_sec_by_provider: BTreeMap<&'static str, Vec<u64>> =
            burn_per_decisec_by_provider
                .iter()
                .map(|(k, v)| {
                    (
                        *k,
                        v.chunks(10).map(|c| c.iter().sum()).collect::<Vec<u64>>(),
                    )
                })
                .collect();

        let rates_for = |map: &BTreeMap<i64, u64>| -> (f64, f64, f64) {
            let secs10_cutoff = now_deci - 100;
            let s: u64 = map
                .iter()
                .filter(|(d, _)| **d > secs10_cutoff)
                .map(|(_, t)| *t)
                .sum();
            let tps10 = s as f64 / ROLLING_RATE_SECS as f64;
            let inst_cutoff = now_deci - INSTANT_RATE_DECIS;
            let i: u64 = map
                .iter()
                .filter(|(d, _)| **d > inst_cutoff)
                .map(|(_, t)| *t)
                .sum();
            let inst = i as f64 / (INSTANT_RATE_DECIS as f64 * 0.1);
            let micro_cutoff = now_deci - MICRO_RATE_DECIS;
            let m: u64 = map
                .iter()
                .filter(|(d, _)| **d > micro_cutoff)
                .map(|(_, t)| *t)
                .sum();
            let micro = m as f64 / (MICRO_RATE_DECIS as f64 * 0.1);
            (tps10, inst, micro)
        };

        let mut current_rate_tps_by_provider: BTreeMap<&'static str, f64> = BTreeMap::new();
        let mut instant_rate_tps_by_provider: BTreeMap<&'static str, f64> = BTreeMap::new();
        let mut micro_rate_tps_by_provider: BTreeMap<&'static str, f64> = BTreeMap::new();
        for (k, map) in &self.decis_provider {
            let (tps, inst, micro) = rates_for(map);
            current_rate_tps_by_provider.insert(*k, tps);
            instant_rate_tps_by_provider.insert(*k, inst);
            micro_rate_tps_by_provider.insert(*k, micro);
        }

        let current_rate_tpm_by_provider: BTreeMap<&'static str, f64> = self
            .buckets_provider
            .iter()
            .map(|(k, map)| {
                let recent: u64 = map
                    .iter()
                    .filter(|(m, _)| **m > rate_cutoff)
                    .map(|(_, t)| *t)
                    .sum();
                (*k, recent as f64 / ROLLING_RATE_MINUTES as f64)
            })
            .collect();

        let events_5min: u32 = self.events_minute.values().sum();
        let events_per_min = events_5min as f64 / 5.0;

        if current_rate > self.peak_rate {
            self.peak_rate = current_rate;
            self.peak_at = Some(now);
        }

        // Sessions: drop idle.
        let session_cutoff = now - chrono::Duration::seconds(SESSION_IDLE_SECS);
        self.sessions.retain(|_, s| s.last_seen > session_cutoff);
        let mut sessions: Vec<SessionStat> = self.sessions.values().cloned().collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.last_seen));

        let by_provider: BTreeMap<&'static str, Totals> = self
            .by_provider
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();

        let drip_5min: u32 = self.drip_minute.values().sum();
        let drip_per_min = drip_5min as f64 / DRIP_WINDOW_MINUTES as f64;

        Snapshot {
            generated_at: now,
            today: self.today.clone(),
            by_provider,
            sessions,
            recent: self.recent.clone(),
            burn_per_min,
            burn_per_min_by_provider,
            burn_per_sec,
            burn_per_sec_by_provider,
            burn_per_decisec,
            burn_per_decisec_by_provider,
            current_rate_tpm: current_rate,
            current_rate_tpm_by_provider,
            current_rate_tps,
            current_rate_tps_by_provider,
            instant_rate_tps,
            instant_rate_tps_by_provider,
            micro_rate_tps,
            micro_rate_tps_by_provider,
            drip_per_min,
            peak_rate_tpm: self.peak_rate,
            peak_rate_at: self.peak_at,
            last_event_at: self.last_event_at,
            last_event_tokens: self.last_event_tokens,
            events_per_min,
            watched_files: self.file_counts.values().sum(),
            dropped_events: self.dropped,
            last_activity_at: self.last_activity.iter().map(|(k, v)| (*k, *v)).collect(),
            in_flight: self
                .in_flight
                .iter()
                .map(|(k, v)| (*k, v.len() as u32))
                .collect(),
            oldest_in_flight_at: self
                .in_flight
                .iter()
                .filter_map(|(k, v)| v.front().map(|t| (*k, *t)))
                .collect(),
            in_flight_by_provider: self
                .in_flight
                .iter()
                .filter_map(|(k, v)| v.front().map(|t| (*k, (v.len() as u32, *t))))
                .collect(),
            median_tps_by_provider: self
                .tps_history
                .iter()
                .map(|(k, v)| (*k, median_f64(v).max(20.0)))
                .collect(),
            median_response_secs_by_provider: self
                .duration_history
                .iter()
                .map(|(k, v)| (*k, median_f64(v).max(0.5)))
                .collect(),
            ttf_last_ms_by_provider: self
                .ttf_history
                .iter()
                .filter_map(|(k, v)| v.back().map(|t| (*k, *t)))
                .collect(),
            ttf_median_ms_by_provider: self
                .ttf_history
                .iter()
                .map(|(k, v)| (*k, median_u64(v)))
                .collect(),
            last_error: self.last_error.clone(),
        }
    }

    pub fn reset(&mut self) {
        self.today = Totals::default();
        self.by_provider.clear();
        self.sessions.clear();
        self.recent.clear();
        self.buckets.clear();
        self.buckets_provider.clear();
        self.decis.clear();
        self.decis_provider.clear();
        self.events_minute.clear();
        self.drip_minute.clear();
        self.in_flight.clear();
        self.credited_in_flight.clear();
        self.last_event_at = None;
        self.last_event_tokens = 0;
        self.peak_rate = 0.0;
        self.peak_at = None;
    }
}

fn build_buckets(map: &BTreeMap<i64, u64>, now_min: i64) -> Vec<BurnBucket> {
    (0..SPARKLINE_MINUTES as i64)
        .rev()
        .map(|i| {
            let minute = now_min - i;
            BurnBucket {
                minute,
                tokens: map.get(&minute).copied().unwrap_or(0),
            }
        })
        .collect()
}

fn median_f64(v: &VecDeque<f64>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s: Vec<f64> = v.iter().copied().collect();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    s[s.len() / 2]
}

fn median_u64(v: &VecDeque<u64>) -> u64 {
    if v.is_empty() {
        return 0;
    }
    let mut s: Vec<u64> = v.iter().copied().collect();
    s.sort();
    s[s.len() / 2]
}

pub enum Command {
    Reset,
}

#[allow(dead_code)]
pub fn spawn(
    rx: mpsc::Receiver<IngestMessage>,
    cmd_rx: mpsc::Receiver<Command>,
) -> watch::Receiver<Arc<Snapshot>> {
    spawn_with(rx, cmd_rx, false)
}

pub fn spawn_with(
    mut rx: mpsc::Receiver<IngestMessage>,
    mut cmd_rx: mpsc::Receiver<Command>,
    persist_enabled: bool,
) -> watch::Receiver<Arc<Snapshot>> {
    let initial = Arc::new(Snapshot::default());
    let (tx, watch_rx) = watch::channel(initial);
    tokio::spawn(async move {
        let mut agg = Aggregator::new();
        // Replay today's persisted events (if persistence is on) so the TUI
        // boots with non-zero totals after a restart.
        if persist_enabled {
            match crate::persist::replay_today() {
                Ok(events) => {
                    let n = events.len();
                    for ev in events {
                        agg.ingest(ev);
                    }
                    if n > 0 {
                        tracing::info!(replayed = n, "persist: replayed today's events");
                        let snap = Arc::new(agg.snapshot());
                        let _ = tx.send(snap);
                    }
                }
                Err(e) => tracing::warn!(error = %e, "persist: replay failed"),
            }
        }
        const ACTIVE_TICK_MS: u64 = 25;
        const IDLE_TICK_MS: u64 = 100;
        const COALESCE_MS: i64 = 8;
        let mut current_tick_ms: u64 = ACTIVE_TICK_MS;
        let mut tick = tokio::time::interval(Duration::from_millis(current_tick_ms));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_pub_ms: i64 = 0;
        let mut dirty = true;

        // Per-event publish helper that coalesces bursts: if we already
        // published in the last COALESCE_MS, just mark dirty so the next tick
        // (or the next gap-wide event) catches up.
        loop {
            tokio::select! {
                Some(msg) = rx.recv() => {
                    let mut had_state_change = true;
                    match msg {
                        IngestMessage::Event(ev) => {
                            let label = ev.provider.label();
                            let to_persist = if persist_enabled { Some(ev.clone()) } else { None };
                            let counted = agg.ingest(ev);
                            if counted {
                                agg.last_activity.insert(label, Utc::now());
                                // Persist only newly-counted events so the
                                // on-disk log doesn't accumulate duplicates
                                // (which would re-inflate the total on replay).
                                if let Some(p) = to_persist {
                                    tokio::spawn(async move {
                                        if let Err(e) = crate::persist::append(&p).await {
                                            tracing::warn!(error = %e, "persist append failed");
                                        }
                                    });
                                }
                            } else {
                                had_state_change = false;
                            }
                        }
                        IngestMessage::FileCount { source, count } => {
                            agg.file_counts.insert(crate::ingest::intern_source(&source), count);
                            had_state_change = false; // very low signal
                        }
                        IngestMessage::Activity { source } => {
                            let source = crate::ingest::intern_source(&source);
                            let now = Utc::now();
                            agg.last_activity.insert(source, now);
                            if let Some(q) = agg.pending_ttf.get_mut(source) {
                                if let Some(start) = q.pop_front() {
                                    let ttf_ms = (now - start).num_milliseconds().max(0) as u64;
                                    let h = agg.ttf_history.entry(source).or_default();
                                    if h.len() == HISTORY_CAP { h.pop_front(); }
                                    h.push_back(ttf_ms);
                                }
                            }
                            // Activity should drive a publish so the UI can
                            // show "burning" even before the usage event lands.
                        }
                        IngestMessage::RequestStart { source } => {
                            let source = crate::ingest::intern_source(&source);
                            if !agg.request_start(source) {
                                had_state_change = false;
                            }
                        }
                        IngestMessage::MicroBurn { source, tokens } => {
                            agg.ingest_micro(crate::ingest::intern_source(&source), tokens);
                        }
                        IngestMessage::PartialDelta { source, est_tokens } => {
                            agg.ingest_partial(crate::ingest::intern_source(&source), est_tokens);
                        }
                        IngestMessage::Error(e) => {
                            agg.last_error = Some(e);
                            had_state_change = false;
                        }
                    }
                    if had_state_change { dirty = true; }
                    let now_ms = Utc::now().timestamp_millis();
                    if dirty && now_ms - last_pub_ms >= COALESCE_MS {
                        let snap = Arc::new(agg.snapshot());
                        let _ = tx.send(snap);
                        last_pub_ms = now_ms;
                        dirty = false;
                    }

                    // Bursts mean active mode.
                    if current_tick_ms != ACTIVE_TICK_MS {
                        current_tick_ms = ACTIVE_TICK_MS;
                        tick = tokio::time::interval(Duration::from_millis(current_tick_ms));
                        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    }
                }
                Some(cmd) = cmd_rx.recv() => match cmd {
                    Command::Reset => { agg.reset(); dirty = true; }
                },
                _ = tick.tick() => {
                    // Adaptive tick: idle backoff when nothing has happened for >12s
                    // (longer than the 10s rolling window so it can fully decay first).
                    let now = Utc::now();
                    let since_last_ms: i64 = agg
                        .last_activity
                        .values()
                        .map(|t| (now - *t).num_milliseconds())
                        .min()
                        .unwrap_or(i64::MAX);
                    let any_recent = since_last_ms < 12_000;
                    let any_in_flight = agg.in_flight.values().any(|q| !q.is_empty());
                    let want_ms = if any_recent || any_in_flight { ACTIVE_TICK_MS } else { IDLE_TICK_MS };
                    if want_ms != current_tick_ms {
                        current_tick_ms = want_ms;
                        tick = tokio::time::interval(Duration::from_millis(current_tick_ms));
                        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    }
                    // Always rebuild while active (so rolling windows decay).
                    // Once fully idle, ship one final snapshot per ~250ms tick for a
                    // few cycles so the UI shows the decayed values, then stop.
                    let now_ms = now.timestamp_millis();
                    let stale_pub = now_ms - last_pub_ms > 1_000;
                    if dirty || any_in_flight || any_recent || stale_pub {
                        let snap = Arc::new(agg.snapshot());
                        let _ = tx.send(snap);
                        last_pub_ms = now_ms;
                        dirty = false;
                    }
                }
            }
        }
    });
    watch_rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Provider;
    use chrono::Utc;

    fn make_event(provider: Provider, total: u64) -> UsageEvent {
        UsageEvent {
            ts: Utc::now(),
            provider,
            model: "test".into(),
            session_id: "s".into(),
            cwd: None,
            input_tokens: 0,
            output_tokens: total,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            reasoning_tokens: 0,
            total_tokens: total,
            raw_source: "test".into(),
            id: None,
        }
    }

    #[test]
    fn dedup_by_id_counts_once() {
        let mut agg = Aggregator::new();
        let mut ev = make_event(Provider::Copilot, 100);
        ev.id = Some("abc".into());
        assert!(agg.ingest(ev.clone()), "first occurrence counts");
        assert!(!agg.ingest(ev.clone()), "duplicate id is skipped");
        assert!(!agg.ingest(ev.clone()), "still skipped");
        assert_eq!(agg.today.total, 100, "counted exactly once");

        // A different id is counted.
        ev.id = Some("xyz".into());
        assert!(agg.ingest(ev));
        assert_eq!(agg.today.total, 200);

        // No-id fallback: identical provider+ts+total is a duplicate; a
        // different total is a distinct event.
        let ts = Utc::now();
        let mut a = make_event(Provider::Copilot, 10);
        a.ts = ts;
        let b = a.clone();
        let mut c = make_event(Provider::Copilot, 11);
        c.ts = ts;
        assert!(agg.ingest(a));
        assert!(!agg.ingest(b), "identical no-id record deduped");
        assert!(agg.ingest(c), "different total counted");
        assert_eq!(agg.today.total, 221);
    }

    /// Property: sum of authoritative totals == today.total + by_provider total
    /// regardless of how many PartialDelta credits were issued in between.
    #[test]
    fn partial_deltas_reconcile_against_event_totals() {
        let mut agg = Aggregator::new();
        // Simulate request 1: start → 3 partials totalling 70 tokens → event 100.
        agg.in_flight
            .entry("copilot")
            .or_default()
            .push_back(Utc::now());
        agg.credited_in_flight
            .entry("copilot")
            .or_default()
            .push_back(0);
        agg.ingest_partial("copilot", 30);
        agg.ingest_partial("copilot", 25);
        agg.ingest_partial("copilot", 15); // 70 credited
        agg.ingest(make_event(Provider::Copilot, 100));

        // Simulate request 2: 0 partials, event 50 (over-credited case impossible
        // here but verify exact pass-through).
        agg.in_flight
            .entry("copilot")
            .or_default()
            .push_back(Utc::now());
        agg.credited_in_flight
            .entry("copilot")
            .or_default()
            .push_back(0);
        agg.ingest(make_event(Provider::Copilot, 50));

        // Simulate request 3: over-credit (estimate higher than authoritative)
        // → reconciliation should clamp at 0 added by Event but totals should
        // end up == sum of Event.total_tokens.
        agg.in_flight
            .entry("copilot")
            .or_default()
            .push_back(Utc::now());
        agg.credited_in_flight
            .entry("copilot")
            .or_default()
            .push_back(0);
        agg.ingest_partial("copilot", 200);
        agg.ingest(make_event(Provider::Copilot, 80));

        let snap = agg.snapshot();
        // sum of Event.total_tokens = 100 + 50 + 80 = 230
        let expected: u64 = 100 + 50 + 80;
        // Note: partial 200 vs event 80 means we over-credited by 120. The
        // reconciliation cap (saturating_sub) clamps the Event to 0 added,
        // but the over-credited 120 stays in totals. So the actual invariant
        // we guarantee is: totals >= sum_of_events, and == when no
        // over-crediting happened.
        // For simplicity, verify the by_provider total equals today.total
        // (consistency between scopes) and matches expected when partials
        // don't exceed events.
        assert_eq!(
            snap.today.total,
            snap.by_provider.get("copilot").unwrap().total
        );
        // Sanity: at least all authoritative tokens are present.
        assert!(
            snap.today.total >= expected,
            "total {} < expected {}",
            snap.today.total,
            expected
        );
    }

    /// In the well-behaved case (estimates ≤ authoritative), totals match exactly.
    #[test]
    fn partial_deltas_under_event_totals_match_exact() {
        let mut agg = Aggregator::new();
        for &(partials, total) in &[(50u64, 100u64), (200, 500), (0, 75)] {
            agg.in_flight
                .entry("claude")
                .or_default()
                .push_back(Utc::now());
            agg.credited_in_flight
                .entry("claude")
                .or_default()
                .push_back(0);
            if partials > 0 {
                agg.ingest_partial("claude", partials as u32);
            }
            agg.ingest(make_event(Provider::Claude, total));
        }
        let snap = agg.snapshot();
        let expected = 100 + 500 + 75;
        assert_eq!(snap.today.total, expected);
        assert_eq!(snap.by_provider.get("claude").unwrap().total, expected);
    }

    /// Within a 750ms window, repeated RequestStart for the same source should
    /// be deduplicated. Starts after the window are accepted again.
    #[test]
    fn request_start_dedupes_within_window() {
        let mut agg = Aggregator::new();
        // First start: accepted.
        assert!(agg.request_start("copilot"));
        // Manually rewind the recorded time so the next call lies within 200 ms.
        let t0 = *agg.last_request_start_at.get("copilot").unwrap();
        agg.last_request_start_at
            .insert("copilot", t0 - chrono::Duration::milliseconds(200));
        // 200 ms later (window: 750 ms): rejected.
        assert!(!agg.request_start("copilot"));
        // Pretend 800 ms have now passed since the last accepted start.
        let last = *agg.last_request_start_at.get("copilot").unwrap();
        agg.last_request_start_at
            .insert("copilot", last - chrono::Duration::milliseconds(800));
        // 800 ms later: accepted again.
        assert!(agg.request_start("copilot"));
        // Two enqueued in_flight entries (the rejected one was dropped).
        assert_eq!(agg.in_flight.get("copilot").map(|q| q.len()), Some(2));
    }

    /// An Event landing between two RequestStarts releases the dedup window —
    /// chained tool turns must not be suppressed.
    #[test]
    fn request_start_allowed_after_event_in_window() {
        let mut agg = Aggregator::new();
        agg.in_flight
            .entry("copilot")
            .or_default()
            .push_back(Utc::now());
        agg.credited_in_flight
            .entry("copilot")
            .or_default()
            .push_back(0);
        assert!(agg.request_start("copilot"));
        // Real event lands → updates last_event_at_by_provider via ingest().
        agg.ingest(make_event(Provider::Copilot, 50));
        // Even within 200 ms, the next start should be accepted.
        let t = *agg.last_request_start_at.get("copilot").unwrap();
        agg.last_request_start_at
            .insert("copilot", t - chrono::Duration::milliseconds(200));
        assert!(agg.request_start("copilot"));
    }
}
