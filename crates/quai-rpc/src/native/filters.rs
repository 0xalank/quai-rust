//! go-quai v0.57.0's filter API in the "quai" namespace
//! (quai/filters/api.go, filter_system.go) over a `ChainView` whose head
//! moves: polling filters (`quai_newFilter`, `quai_newBlockFilter`,
//! `quai_newPendingTransactionFilter`, `quai_getFilterChanges`,
//! `quai_getFilterLogs`, `quai_uninstallFilter`) and the WebSocket
//! subscriptions "newHeads", "logs" and "newPendingTransactions".
//!
//! Events follow what go-quai's core sends (core/headerchain.go
//! `SetCurrentHeader` and `AppendBlock`, core/slice.go
//! `GeneratePendingHeader`): when the head moves, every block that became
//! canonical sends its logs, oldest first, and then the new head sends one
//! head event. A jump of several blocks or a reorg therefore gives logs for
//! each new block and a single head. go-quai v0.57.0 never sends
//! `RemovedLogsEvent` (nothing writes to `BodyDb.rmLogsFeed`), so a reorg
//! sends no `"removed": true` logs; `ChainView::removed_logs` is not read.
//! Nothing writes to the event system's `txsCh` either: pending-transaction
//! filters and "newPendingTransactions" never receive anything.
//!
//! Which filters receive what (filter_system.go): only `LogsSubscription`
//! filters get mined logs (`handleLogs`), with the from/to block checked per
//! log and the block hash of a criteria ignored; `PendingLogsSubscription`
//! gets the miner's pending logs (this node has no miner) and
//! `MinedAndPendingLogsSubscription` has no handler at all. Block filters
//! and "newHeads" get the head events.
//!
//! go-quai delivers events with non-blocking sends into channels of 0 or 10
//! entries and drops what does not fit. This port queues them: a
//! subscription's connection holds up to `OUTBOX` messages, beyond which
//! notifications are dropped as Go drops them.
use super::args::{self, Args, BlockNumber};
use super::{filter, filter_logs, json, matches, Filter, RpcError, MAX_FILTER_RANGE};
use crate::view::{BlockData, ChainView, LogData};
use quai_primitives::Hash;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

type R<T> = Result<T, RpcError>;

/// go-quai's filter deadline (quai/backend.go: `5*time.Minute`).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
/// go-quai's `--rpc.ws-max-subs` default.
pub const DEFAULT_SUBSCRIPTION_LIMIT: usize = 1000;
/// Messages a WebSocket connection may have queued before notifications
/// are dropped.
pub const OUTBOX: usize = 4096;
/// The canonical hashes the event loop remembers, to find where a reorg
/// starts.
const REMEMBERED: usize = 512;
/// The most blocks one head move sends logs for.
const MAX_WALK: usize = 10_000;

/// The filter methods of this module.
pub const METHODS: &[&str] = &[
    "quai_newPendingTransactionFilter",
    "quai_newBlockFilter",
    "quai_newFilter",
    "quai_uninstallFilter",
    "quai_getFilterChanges",
    "quai_getFilterLogs",
];

/// What an event-system subscription receives.
#[derive(Clone, Debug)]
enum Event {
    /// `LogsSubscription`: mined logs that match the criteria.
    Logs(Filter),
    /// `PendingLogsSubscription` (the miner's pending logs; no miner here)
    /// and `MinedAndPendingLogsSubscription` (no handler in go-quai).
    Silent,
    /// `PendingTransactionsSubscription`: nothing sends to `txsCh`.
    PendingTxs,
    /// `ChainHeadSubscription`: block filters and "newHeads".
    Heads,
}

/// `EventSystem.SubscribeLogs`: the subscription type of a criteria.
fn subscribe_logs(crit: &Filter) -> Result<Event, String> {
    let from = crit.from.map_or(-1, BlockNumber::int64);
    let to = crit.to.map_or(-1, BlockNumber::int64);
    if from >= 0 && to >= 0 && to > from && to - from > MAX_FILTER_RANGE {
        return Err(format!(
            "invalid from and to block combination: block range > {MAX_FILTER_RANGE}"
        ));
    }
    if from == -2 && to == -2 {
        return Ok(Event::Silent);
    }
    if from == -1 && to == -1 {
        return Ok(Event::Logs(crit.clone()));
    }
    if from >= 0 && to >= 0 && to >= from {
        return Ok(Event::Logs(crit.clone()));
    }
    if from >= -1 && to == -2 {
        return Ok(Event::Silent);
    }
    if from >= 0 && to == -1 {
        return Ok(Event::Logs(crit.clone()));
    }
    Err("invalid from and to block combination: from > to".into())
}

/// `filterLogs` of the event system: the criteria's from and to block, then
/// its addresses and topics.
fn event_matches(f: &Filter, log: &LogData, number: u64) -> bool {
    if let Some(from) = f.from.map(BlockNumber::int64) {
        if from >= 0 && from as u64 > number {
            return false;
        }
    }
    if let Some(to) = f.to.map(BlockNumber::int64) {
        if to >= 0 && (to as u64) < number {
            return false;
        }
    }
    matches(f, log)
}

/// The type of a polling filter in `api.filters`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PollType {
    PendingTxs,
    Blocks,
    Logs,
}

struct Poll {
    typ: PollType,
    event: Event,
    crit: Option<Filter>,
    deadline: Instant,
    hashes: Vec<Hash>,
    logs: Vec<Value>,
}

struct Sub {
    conn: u64,
    event: Event,
    out: mpsc::Sender<String>,
    /// Notifications made before the subscribe answer went out
    /// (`Notifier.buffer`); None once active.
    buffer: Option<Vec<String>>,
}
impl Sub {
    fn notify(&mut self, id: &str, result: &Value) {
        let text = notification(id, result);
        match &mut self.buffer {
            Some(b) => b.push(text),
            None => {
                let _ = self.out.try_send(text);
            }
        }
    }
}

/// The JSON text of a notification: go-quai's `Notifier.send`, written by
/// `json.Encoder` (with its newline).
pub fn notification(id: &str, result: &Value) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"method\":\"quai_subscription\",\"params\":{{\"subscription\":{},\"result\":{}}}}}\n",
        Value::String(id.to_string()),
        result
    )
}

#[derive(Default)]
struct State {
    polls: HashMap<String, Poll>,
    subs: HashMap<String, Sub>,
    /// `activeSubscriptions`.
    active: usize,
    /// The canonical chain the event loop last saw (recent part).
    chain: BTreeMap<u64, Hash>,
    /// Head events handled so far.
    handled: u64,
}

/// `rpc.NewID`: 16 random bytes in hex without leading zeros.
struct IdGen {
    keys: std::collections::hash_map::RandomState,
    counter: AtomicU64,
}
impl IdGen {
    fn new() -> Self {
        Self {
            keys: std::collections::hash_map::RandomState::new(),
            counter: AtomicU64::new(0),
        }
    }
    fn next(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let mut bytes = [0u8; 16];
        for (half, chunk) in bytes.chunks_mut(8).enumerate() {
            let mut h = self.keys.build_hasher();
            h.write_u64(n);
            h.write_u8(half as u8);
            chunk.copy_from_slice(&h.finish().to_be_bytes());
        }
        encode_id(&bytes)
    }
}
/// `rpc.encodeID`.
pub fn encode_id(bytes: &[u8]) -> String {
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let trimmed = hex.trim_start_matches('0');
    format!("0x{}", if trimmed.is_empty() { "0" } else { trimmed })
}

/// `rpc.ID` arguments: a JSON string (null decodes to "").
pub(crate) fn rpc_id(v: &Value) -> Result<String, String> {
    let kind = match v {
        Value::String(s) => return Ok(s.clone()),
        Value::Null => return Ok(String::new()),
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    Err(args::type_error(kind, "rpc.ID"))
}

/// The filter API of one node (`PublicFilterAPI` with its `EventSystem`).
pub struct FilterApi {
    view: Arc<dyn ChainView>,
    timeout: Duration,
    limit: usize,
    state: Mutex<State>,
    ids: IdGen,
    started: AtomicBool,
}

impl FilterApi {
    pub fn new(view: Arc<dyn ChainView>, timeout: Duration, limit: usize) -> Arc<Self> {
        let api = Arc::new(Self {
            view,
            timeout,
            limit,
            state: Mutex::new(State::default()),
            ids: IdGen::new(),
            started: AtomicBool::new(false),
        });
        let view = api.view.pinned().unwrap_or_else(|| api.view.clone());
        let head = view.head();
        if let Ok(Some(b)) = view.block_by_number(head) {
            api.lock().chain.insert(head, b.hash);
        }
        api
    }
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Start the event loop (head events of the view) and the timeout loop
    /// on the current Tokio runtime, once.
    pub fn start(self: &Arc<Self>) {
        if tokio::runtime::Handle::try_current().is_err()
            || self.started.swap(true, Ordering::SeqCst)
        {
            return;
        }
        if let Some(mut events) = self.view.head_events() {
            let api = Arc::downgrade(self);
            tokio::spawn(async move {
                use tokio::sync::broadcast::error::RecvError;
                loop {
                    let hash = match events.recv().await {
                        Ok(h) => Some(h),
                        // Lagged: go on from the current head.
                        Err(RecvError::Lagged(_)) => None,
                        Err(RecvError::Closed) => return,
                    };
                    let Some(api) = api.upgrade() else { return };
                    let _ = tokio::task::spawn_blocking(move || api.on_head(hash)).await;
                }
            });
        }
        // `timeoutLoop`: a ticker of `timeout` removes the filters whose
        // deadline passed.
        let api = Arc::downgrade(self);
        let timeout = self.timeout;
        tokio::spawn(async move {
            let mut ticker =
                tokio::time::interval_at(tokio::time::Instant::now() + timeout, timeout);
            loop {
                ticker.tick().await;
                let Some(api) = api.upgrade() else { return };
                let now = Instant::now();
                api.lock().polls.retain(|_, f| f.deadline > now);
            }
        });
    }

    /// The polling filter methods. None: `method` is not one of them.
    pub async fn call(self: &Arc<Self>, method: &str, params: &Value) -> Option<R<Value>> {
        if !METHODS.contains(&method) {
            return None;
        }
        self.start();
        Some(match method {
            "quai_getFilterLogs" => {
                let api = self.clone();
                let params = params.clone();
                match tokio::task::spawn_blocking(move || api.get_filter_logs(&params)).await {
                    Ok(r) => r,
                    Err(_) => Err(RpcError::go("method handler crashed")),
                }
            }
            _ => self.call_sync(method, params),
        })
    }

    fn install(&self, typ: PollType, event: Event, crit: Option<Filter>) -> String {
        let id = self.ids.next();
        self.lock().polls.insert(
            id.clone(),
            Poll {
                typ,
                event,
                crit,
                deadline: Instant::now() + self.timeout,
                hashes: vec![],
                logs: vec![],
            },
        );
        id
    }

    /// Every filter method except `quai_getFilterLogs`.
    pub fn call_sync(&self, method: &str, params: &Value) -> R<Value> {
        match method {
            "quai_newPendingTransactionFilter" => {
                Args::new(params, 0)?;
                Ok(Value::String(self.install(
                    PollType::PendingTxs,
                    Event::PendingTxs,
                    None,
                )))
            }
            "quai_newBlockFilter" => {
                Args::new(params, 0)?;
                Ok(Value::String(self.install(
                    PollType::Blocks,
                    Event::Heads,
                    None,
                )))
            }
            "quai_newFilter" => {
                let a = Args::new(params, 1)?;
                let crit = a.get(0, filter)?;
                let event = subscribe_logs(&crit).map_err(RpcError::go)?;
                Ok(Value::String(self.install(
                    PollType::Logs,
                    event,
                    Some(crit),
                )))
            }
            "quai_uninstallFilter" => {
                let a = Args::new(params, 1)?;
                let id = a.get(0, rpc_id)?;
                Ok(Value::Bool(self.lock().polls.remove(&id).is_some()))
            }
            "quai_getFilterChanges" => {
                let a = Args::new(params, 1)?;
                let id = a.get(0, rpc_id)?;
                let mut st = self.lock();
                let Some(f) = st.polls.get_mut(&id) else {
                    return Err(RpcError::go("filter not found"));
                };
                f.deadline = Instant::now() + self.timeout;
                Ok(match f.typ {
                    PollType::PendingTxs | PollType::Blocks => Value::Array(
                        std::mem::take(&mut f.hashes)
                            .iter()
                            .map(|h| json::hash_hex(h))
                            .collect(),
                    ),
                    PollType::Logs => Value::Array(std::mem::take(&mut f.logs)),
                })
            }
            "quai_getFilterLogs" => self.get_filter_logs(params),
            _ => Err(RpcError::method_not_found(method)),
        }
    }

    /// `GetFilterLogs`: run the filter's criteria as a range or block
    /// filter. It does not reset the deadline.
    fn get_filter_logs(&self, params: &Value) -> R<Value> {
        let a = Args::new(params, 1)?;
        let id = a.get(0, rpc_id)?;
        let crit = {
            let st = self.lock();
            match st.polls.get(&id) {
                Some(Poll {
                    typ: PollType::Logs,
                    crit: Some(c),
                    ..
                }) => c.clone(),
                _ => return Err(RpcError::go("filter not found")),
            }
        };
        let view = self.view.pinned().unwrap_or_else(|| self.view.clone());
        filter_logs(view.as_ref(), &crit)
    }

    /// `quai_subscribe` on connection `conn`: the subscription ID. The
    /// subscription buffers its notifications until `activate`.
    pub fn subscribe(
        &self,
        conn: u64,
        out: &mpsc::Sender<String>,
        method: &str,
        params: &Value,
    ) -> R<String> {
        let name = subscription_name(params)?;
        let namespace = method.split('_').next().unwrap_or_default();
        let arity = match (namespace, name.as_str()) {
            ("quai", "newHeads" | "newPendingTransactions") => 1,
            ("quai", "logs") => 2,
            _ => {
                return Err(RpcError {
                    code: -32601,
                    message: format!(
                        "no {} subscription in {namespace} namespace",
                        go_quote(&name)
                    ),
                    data: None,
                })
            }
        };
        let a = Args::new(params, arity)?;
        a.get(0, args::string)?;
        let crit = if arity == 2 {
            Some(a.get(1, filter)?)
        } else {
            None
        };
        let mut st = self.lock();
        if st.active >= self.limit {
            return Err(RpcError::go("too many subscribers"));
        }
        let event = match name.as_str() {
            "newHeads" => Event::Heads,
            "newPendingTransactions" => Event::PendingTxs,
            _ => subscribe_logs(crit.as_ref().ok_or_else(|| RpcError::go("logs criteria"))?)
                .map_err(RpcError::go)?,
        };
        let id = self.ids.next();
        st.active += 1;
        st.subs.insert(
            id.clone(),
            Sub {
                conn,
                event,
                out: out.clone(),
                buffer: Some(vec![]),
            },
        );
        Ok(id)
    }
    /// `Notifier.activate`: send the buffered notifications; later ones go
    /// out directly.
    pub fn activate(&self, id: &str) {
        let mut st = self.lock();
        if let Some(sub) = st.subs.get_mut(id) {
            for text in sub.buffer.take().unwrap_or_default() {
                let _ = sub.out.try_send(text);
            }
        }
    }
    /// `*_unsubscribe` on connection `conn` (`handler.unsubscribe`).
    pub fn unsubscribe(&self, conn: u64, params: &Value) -> R<Value> {
        let a = Args::new(params, 1)?;
        let id = a.get(0, rpc_id)?;
        let mut st = self.lock();
        match st.subs.get(&id) {
            Some(s) if s.conn == conn => {
                st.subs.remove(&id);
                st.active = st.active.saturating_sub(1);
                Ok(Value::Bool(true))
            }
            _ => Err(RpcError::go("subscription not found")),
        }
    }
    /// The connection closed: end its subscriptions.
    pub fn close(&self, conn: u64) {
        let mut st = self.lock();
        let before = st.subs.len();
        st.subs.retain(|_, s| s.conn != conn);
        let ended = before - st.subs.len();
        st.active = st.active.saturating_sub(ended);
    }

    /// One head event: send the logs of the blocks that became canonical,
    /// oldest first, then the head. `hash` None: the view's current head.
    pub fn on_head(&self, hash: Option<Hash>) {
        let view = self.view.pinned().unwrap_or_else(|| self.view.clone());
        let head = match hash {
            Some(h) => view.block_by_hash(&h),
            None => view.block_by_number(view.head()),
        };
        // A head that a later reorg already replaced sends nothing.
        let Ok(Some(head)) = head else {
            self.lock().handled += 1;
            return;
        };
        let (known, wants_logs) = {
            let mut st = self.lock();
            if st.chain.get(&head.number) == Some(&head.hash) {
                st.handled += 1;
                return;
            }
            let wants_logs = st.polls.values().any(|p| matches!(p.event, Event::Logs(_)))
                || st.subs.values().any(|s| matches!(s.event, Event::Logs(_)));
            (st.chain.clone(), wants_logs)
        };
        // The new canonical blocks, newest first, down to the common
        // ancestor with the chain this loop saw last.
        let lowest = known.keys().next().copied().unwrap_or(head.number);
        let mut fresh: Vec<(u64, Hash)> = vec![(head.number, head.hash)];
        let mut ancestor = None;
        let mut n = head.number;
        while n > 0 && n > lowest && fresh.len() < MAX_WALK {
            let Ok(Some(hash)) = view.canonical_hash(n - 1) else {
                break;
            };
            if known.get(&(n - 1)) == Some(&hash) {
                ancestor = Some(n - 1);
                break;
            }
            fresh.push((n - 1, hash));
            n -= 1;
        }
        fresh.reverse();
        // The logs of each new block (go-quai's `AppendBlock` → logsFeed).
        let mut block_logs: Vec<(Arc<BlockData>, Vec<(LogData, Value)>)> = vec![];
        if wants_logs {
            for (number, hash) in &fresh {
                let b = if *hash == head.hash {
                    head.clone()
                } else {
                    match view.block_by_number(*number) {
                        Ok(Some(b)) if b.hash == *hash => b,
                        _ => continue,
                    }
                };
                let b = &b;
                let Ok(receipts) = view.receipts(b) else {
                    continue;
                };
                let mut list = vec![];
                let mut index = 0;
                for (i, r) in receipts.iter().enumerate() {
                    for l in &r.logs {
                        if i < b.tx_hashes.len() {
                            list.push((l.clone(), json::log(l, b, i, index)));
                        }
                        index += 1;
                    }
                }
                if !list.is_empty() {
                    block_logs.push((b.clone(), list));
                }
            }
        }
        let mut st = self.lock();
        // Remember the new canonical chain.
        let keep_below = ancestor.map_or(fresh[0].0, |a| a + 1);
        st.chain.retain(|n, _| *n < keep_below);
        for (n, h) in &fresh {
            st.chain.insert(*n, *h);
        }
        while st.chain.len() > REMEMBERED {
            let Some((&n, _)) = st.chain.iter().next() else {
                break;
            };
            st.chain.remove(&n);
        }
        let State { polls, subs, .. } = &mut *st;
        for (b, logs) in &block_logs {
            for p in polls.values_mut() {
                if let Event::Logs(f) = &p.event {
                    p.logs.extend(
                        logs.iter()
                            .filter(|(l, _)| event_matches(f, l, b.number))
                            .map(|(_, v)| v.clone()),
                    );
                }
            }
            for (id, s) in subs.iter_mut() {
                if let Event::Logs(f) = &s.event {
                    let matched: Vec<&Value> = logs
                        .iter()
                        .filter(|(l, _)| event_matches(f, l, b.number))
                        .map(|(_, v)| v)
                        .collect();
                    for v in matched {
                        s.notify(id, v);
                    }
                }
            }
        }
        // The head event (go-quai's `chainHeadFeed`).
        for p in polls.values_mut() {
            if matches!(p.event, Event::Heads) {
                p.hashes.push(head.hash);
            }
        }
        let mut head_json = None;
        for (id, s) in subs.iter_mut() {
            if matches!(s.event, Event::Heads) {
                let v = head_json.get_or_insert_with(|| json::work_object(&head));
                s.notify(id, v);
            }
        }
        st.handled += 1;
    }
    /// The number of head events handled (tests wait on it).
    pub fn handled(&self) -> u64 {
        self.lock().handled
    }
}

/// `parseSubscriptionName`: the first element of the array of parameters.
fn subscription_name(params: &Value) -> R<String> {
    let Value::Array(list) = params else {
        return Err(RpcError::params("non-array args"));
    };
    match list.first() {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => Err(RpcError::params(
            "expected subscription name as first argument",
        )),
    }
}

/// Go's `%q` of a string (`strconv.Quote`).
fn go_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{7}' => out.push_str("\\a"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\u{b}' => out.push_str("\\v"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ids_have_go_form() {
        assert_eq!(encode_id(&[0; 16]), "0x0");
        assert_eq!(encode_id(&[0, 1, 0xab]), "0x1ab");
        let ids = IdGen::new();
        let a = ids.next();
        let b = ids.next();
        assert_ne!(a, b);
        for id in [a, b] {
            assert!(id.starts_with("0x") && id.len() <= 34, "{id}");
            assert!(!id[2..].starts_with('0') || id == "0x0", "{id}");
        }
    }
    #[test]
    fn criteria_types_follow_subscribe_logs() {
        let crit = |from: Option<BlockNumber>, to: Option<BlockNumber>| Filter {
            block_hash: None,
            from,
            to,
            addresses: vec![],
            topics: vec![],
        };
        use BlockNumber::*;
        let kind = |f, t| match subscribe_logs(&crit(f, t)) {
            Ok(Event::Logs(_)) => "logs",
            Ok(Event::Silent) => "silent",
            Ok(_) => "other",
            Err(e) => {
                if e.contains("range") {
                    "range"
                } else {
                    "order"
                }
            }
        };
        assert_eq!(kind(None, None), "logs");
        assert_eq!(kind(Some(Pending), Some(Pending)), "silent");
        assert_eq!(kind(Some(Latest), Some(Pending)), "silent");
        assert_eq!(kind(Some(Number(5)), Some(Pending)), "silent");
        assert_eq!(kind(Some(Number(5)), None), "logs");
        assert_eq!(kind(Some(Number(2)), Some(Number(6))), "logs");
        assert_eq!(kind(Some(Number(6)), Some(Number(2))), "order");
        assert_eq!(kind(Some(Latest), Some(Number(5))), "order");
        assert_eq!(kind(Some(Pending), Some(Number(5))), "order");
        assert_eq!(kind(Some(Number(0)), Some(Number(10_001))), "range");
        assert_eq!(kind(Some(Number(1)), Some(Number(10_001))), "logs");
    }
    #[test]
    fn quotes_like_go() {
        assert_eq!(go_quote("newHeads"), "\"newHeads\"");
        assert_eq!(go_quote("a\"b\n"), "\"a\\\"b\\n\"");
    }
}
