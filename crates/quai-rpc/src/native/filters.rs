//! go-quai v0.57.0's filter API, registered in the "quai" and "eth"
//! namespaces (quai/filters/api.go, filter_system.go), over a `ChainView`
//! whose head moves: polling filters (`quai_newFilter`,
//! `quai_newBlockFilter`, `quai_newPendingTransactionFilter`,
//! `quai_getFilterChanges`, `quai_getFilterLogs`, `quai_uninstallFilter`)
//! and the WebSocket subscriptions "newHeads", "newHeadsV2", "logs",
//! "newPendingTransactions", "newChainBlocks", "newChainBlocksV2" and
//! "accesses" (`quai_subscribe` or `eth_subscribe`; notifications come as
//! `<namespace>_subscription`). The mining subscriptions ("pendingHeader",
//! "blockTemplateUpdates", "newWorkshares", "newWorksharesV2") are not
//! served.
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
use super::{filter, filter_logs_cancellable, json, matches, Filter, RpcError, MAX_FILTER_RANGE};
use crate::view::{BlockData, ChainView, LogData};
use quai_primitives::Hash;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

type R<T> = Result<T, RpcError>;
/// A new canonical block and its logs, each with its JSON.
type BlockLogs = (Arc<BlockData>, Vec<(LogData, Value)>);

/// go-quai's filter deadline (quai/backend.go: `5*time.Minute`).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
/// go-quai's `--rpc.ws-max-subs` default.
pub const DEFAULT_SUBSCRIPTION_LIMIT: usize = 1000;
/// Maximum active HTTP polling filters (`go-quai` `maxPollingFilters`).
pub const MAX_POLLING_FILTERS: usize = 256;
/// Maximum queued items per polling filter before overflowing (`go-quai`
/// `maxPendingFilterItems`).
pub const MAX_PENDING_FILTER_ITEMS: usize = 4096;
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

/// The WebSocket subscriptions of this module (the names `quai_subscribe`
/// and `eth_subscribe` take).
pub const SUBSCRIPTIONS: &[&str] = &[
    "newHeads",
    "newHeadsV2",
    "logs",
    "newPendingTransactions",
    "newChainBlocks",
    "newChainBlocksV2",
    "accesses",
    "pendingHeader",
    "blockTemplateUpdates",
    "newWorkshares",
    "newWorksharesV2",
];

/// What an event-system subscription receives.
#[derive(Clone, Debug)]
enum Event {
    /// `LogsSubscription`: mined logs that match the criteria.
    Logs(Filter),
    /// `PendingLogsSubscription` and `MinedAndPendingLogsSubscription`.
    Silent,
    /// `PendingTransactionsSubscription`: nothing sends to `txsCh`.
    PendingTxs,
    /// `ChainHeadSubscription`: block filters and "newHeads".
    Heads,
    /// "newHeadsV2": the head with RPC version "v2".
    HeadsV2,
    /// `BlocksSubscription`: every appended block ("newChainBlocks", and
    /// with `v2` "newChainBlocksV2").
    ChainBlocks { v2: bool },
    /// "accesses": the heads whose transactions touch the address, and the
    /// blocks that unlock its lockups.
    Accesses(super::args::Address),
    /// "pendingHeader": new pending work object headers (protobuf bytes hex).
    PendingHeader,
    /// "blockTemplateUpdates": new AuxPoW block template JSON for requested params.
    BlockTemplateUpdates(Option<Value>),
    /// "newWorkshares" / "newWorksharesV2": newly received/mined workshares.
    Workshares { _v2: bool },
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
    overflow: bool,
}

struct Sub {
    conn: u64,
    /// "quai" or "eth": the notifications' `<namespace>_subscription`.
    namespace: &'static str,
    event: Event,
    out: mpsc::Sender<String>,
    /// Notifications made before the subscribe answer went out
    /// (`Notifier.buffer`); None once active.
    buffer: Option<Vec<String>>,
}
impl Sub {
    fn notify(&mut self, id: &str, result: &Value) {
        let text = notification_in(self.namespace, id, result);
        match &mut self.buffer {
            Some(b) => {
                if b.len() < OUTBOX {
                    b.push(text);
                }
            }
            None => {
                let _ = self.out.try_send(text);
            }
        }
    }
}

/// The JSON text of a "quai" notification: go-quai's `Notifier.send`,
/// written by `json.Encoder` (with its newline).
pub fn notification(id: &str, result: &Value) -> String {
    notification_in("quai", id, result)
}
/// A notification of a subscription made in `namespace`.
pub fn notification_in(namespace: &str, id: &str, result: &Value) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"method\":\"{namespace}_subscription\",\"params\":{{\"subscription\":{},\"result\":{}}}}}\n",
        Value::String(id.to_string()),
        result
    )
}

#[derive(Default)]
struct State {
    polls: HashMap<String, Poll>,
    subs: HashMap<String, Sub>,
    /// The canonical chain the event loop last saw (recent part).
    chain: BTreeMap<u64, Hash>,
    /// The head of the last head event.
    last: Option<Hash>,
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
    /// Shared active subscription counter across namespaces (`quai` and `eth`).
    active: Arc<AtomicUsize>,
    state: Mutex<State>,
    ids: IdGen,
    started: AtomicBool,
}

impl FilterApi {
    pub fn new(view: Arc<dyn ChainView>, timeout: Duration, limit: usize) -> Arc<Self> {
        Self::with_shared_active(view, timeout, limit, Arc::new(AtomicUsize::new(0)))
    }
    pub fn with_shared_active(
        view: Arc<dyn ChainView>,
        timeout: Duration,
        limit: usize,
        active: Arc<AtomicUsize>,
    ) -> Arc<Self> {
        let api = Arc::new(Self {
            view,
            timeout,
            limit,
            active,
            state: Mutex::new(State::default()),
            ids: IdGen::new(),
            started: AtomicBool::new(false),
        });
        let view = api.view.pinned().unwrap_or_else(|| api.view.clone());
        let head = view.head();
        if let Ok(Some(b)) = view.block_by_number(head) {
            let mut st = api.lock();
            st.chain.insert(head, b.hash);
            st.last = Some(b.hash);
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
        if let Some(mut events) = self.view.chain_events() {
            let api = Arc::downgrade(self);
            tokio::spawn(async move {
                use tokio::sync::broadcast::error::RecvError;
                loop {
                    let hash = match events.recv().await {
                        Ok(h) => h,
                        // Lagged: go-quai drops events a busy subscriber
                        // cannot take.
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => return,
                    };
                    let Some(api) = api.upgrade() else { return };
                    let _ = tokio::task::spawn_blocking(move || api.on_chain_block(hash)).await;
                }
            });
        }
        if let Some(mut events) = self.view.unlock_events() {
            let api = Arc::downgrade(self);
            tokio::spawn(async move {
                use tokio::sync::broadcast::error::RecvError;
                loop {
                    let unlocks = match events.recv().await {
                        Ok(u) => u,
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => return,
                    };
                    let Some(api) = api.upgrade() else { return };
                    api.on_unlocks(&unlocks);
                }
            });
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
        if let Some(mut events) = self.view.pending_header_events() {
            let api = Arc::downgrade(self);
            tokio::spawn(async move {
                use tokio::sync::broadcast::error::RecvError;
                loop {
                    let raw = match events.recv().await {
                        Ok(b) => b,
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => return,
                    };
                    let Some(api) = api.upgrade() else { return };
                    api.on_pending_header(&raw);
                }
            });
        }
        if let Some(mut events) = self.view.template_update_events() {
            let api = Arc::downgrade(self);
            tokio::spawn(async move {
                use tokio::sync::broadcast::error::RecvError;
                loop {
                    let _pow_id = match events.recv().await {
                        Ok(p) => p,
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => return,
                    };
                    let Some(api) = api.upgrade() else { return };
                    let _ = tokio::task::spawn_blocking(move || api.on_template_update()).await;
                }
            });
        }
        if let Some(mut events) = self.view.workshare_events() {
            let api = Arc::downgrade(self);
            tokio::spawn(async move {
                use tokio::sync::broadcast::error::RecvError;
                loop {
                    let wo = match events.recv().await {
                        Ok(w) => w,
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => return,
                    };
                    let Some(api) = api.upgrade() else { return };
                    api.on_workshare(&wo);
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
                match tokio::task::spawn_blocking(move || api.get_filter_logs(&params, None)).await
                {
                    Ok(r) => r,
                    Err(_) => Err(RpcError::go("method handler crashed")),
                }
            }
            _ => self.call_sync(method, params),
        })
    }

    /// Same as [`FilterApi::call`], holding `permit` inside `spawn_blocking`
    /// for `quai_getFilterLogs` and checking `cancel` cooperatively.
    pub async fn call_cancellable(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        permit: tokio::sync::OwnedSemaphorePermit,
        cancel: Arc<AtomicBool>,
    ) -> R<Value> {
        self.start();
        match method {
            "quai_getFilterLogs" => {
                let api = self.clone();
                let params = params.clone();
                match tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    api.get_filter_logs(&params, Some(cancel.as_ref()))
                })
                .await
                {
                    Ok(r) => r,
                    Err(_) => Err(RpcError::go("method handler crashed")),
                }
            }
            _ => {
                let _permit = permit;
                self.call_sync(method, params)
            }
        }
    }

    fn install(&self, typ: PollType, event: Event, crit: Option<Filter>) -> R<String> {
        let now = Instant::now();
        let mut st = self.lock();
        if st.polls.len() >= MAX_POLLING_FILTERS {
            st.polls.retain(|_, f| f.deadline > now);
            if st.polls.len() >= MAX_POLLING_FILTERS {
                return Err(RpcError::go("too many polling filters"));
            }
        }
        let id = self.ids.next();
        st.polls.insert(
            id.clone(),
            Poll {
                typ,
                event,
                crit,
                deadline: now + self.timeout,
                hashes: vec![],
                logs: vec![],
                overflow: false,
            },
        );
        Ok(id)
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
                )?))
            }
            "quai_newBlockFilter" => {
                Args::new(params, 0)?;
                Ok(Value::String(self.install(
                    PollType::Blocks,
                    Event::Heads,
                    None,
                )?))
            }
            "quai_newFilter" => {
                let a = Args::new(params, 1)?;
                let crit = a.get(0, filter)?;
                let event = subscribe_logs(&crit).map_err(RpcError::go)?;
                Ok(Value::String(self.install(
                    PollType::Logs,
                    event,
                    Some(crit),
                )?))
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
                f.overflow = false;
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
            "quai_getFilterLogs" => self.get_filter_logs(params, None),
            _ => Err(RpcError::method_not_found(method)),
        }
    }

    /// `GetFilterLogs`: run the filter's criteria as a range or block
    /// filter. It does not reset the deadline.
    fn get_filter_logs(&self, params: &Value, cancel: Option<&AtomicBool>) -> R<Value> {
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
        filter_logs_cancellable(view.as_ref(), &crit, cancel)
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
        let space: &'static str = if namespace == "eth" { "eth" } else { "quai" };
        let arity = match (namespace, name.as_str()) {
            (
                "quai" | "eth",
                "newHeads"
                | "newHeadsV2"
                | "newPendingTransactions"
                | "newChainBlocks"
                | "newChainBlocksV2"
                | "pendingHeader"
                | "newWorkshares"
                | "newWorksharesV2",
            ) => 1,
            ("quai" | "eth", "logs" | "accesses" | "blockTemplateUpdates") => 2,
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
        let crit = if name == "logs" {
            Some(a.get(1, filter)?)
        } else {
            None
        };
        let accessed = if name == "accesses" {
            Some(a.get(1, args::address)?)
        } else {
            None
        };
        let template_req = if name == "blockTemplateUpdates" {
            params
                .as_array()
                .and_then(|arr| arr.get(1))
                .filter(|v| !v.is_null())
                .cloned()
        } else {
            None
        };
        let mut st = self.lock();
        // Reserve one slot in the shared subscription counter BEFORE checking
        // event criteria (go-quai enforces the limit first), releasing it on
        // error via a drop guard.
        loop {
            let cur = self.active.load(Ordering::Acquire);
            if cur >= self.limit {
                return Err(RpcError::go("too many subscribers"));
            }
            if self
                .active
                .compare_exchange(cur, cur + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        struct SlotGuard<'a> {
            active: &'a AtomicUsize,
            armed: bool,
        }
        impl Drop for SlotGuard<'_> {
            fn drop(&mut self) {
                if self.armed {
                    self.active.fetch_sub(1, Ordering::AcqRel);
                }
            }
        }
        let mut slot = SlotGuard {
            active: &self.active,
            armed: true,
        };
        let event = match name.as_str() {
            "newHeads" => Event::Heads,
            "newHeadsV2" => Event::HeadsV2,
            "newPendingTransactions" => Event::PendingTxs,
            "newChainBlocks" => Event::ChainBlocks { v2: false },
            "newChainBlocksV2" => Event::ChainBlocks { v2: true },
            "pendingHeader" => Event::PendingHeader,
            "blockTemplateUpdates" => Event::BlockTemplateUpdates(template_req),
            "newWorkshares" => Event::Workshares { _v2: false },
            "newWorksharesV2" => Event::Workshares { _v2: true },
            "accesses" => {
                // `addr.InternalAddress()`.
                let address = accessed.unwrap_or_default();
                if !super::internal(self.view.as_ref(), &address) {
                    return Err(RpcError::go("Address belongs to other zone"));
                }
                Event::Accesses(address)
            }
            _ => subscribe_logs(crit.as_ref().ok_or_else(|| RpcError::go("logs criteria"))?)
                .map_err(RpcError::go)?,
        };
        let id = self.ids.next();
        slot.armed = false;
        st.subs.insert(
            id.clone(),
            Sub {
                conn,
                namespace: space,
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
    /// Cancel a subscription whose subscribe response was never delivered
    /// (for example when a batch hit `-32005 response too large`).
    pub fn cancel(&self, id: &str) {
        let mut st = self.lock();
        if st.subs.remove(id).is_some() {
            self.active.fetch_sub(1, Ordering::AcqRel);
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
                self.active.fetch_sub(1, Ordering::AcqRel);
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
        if ended > 0 {
            self.active.fetch_sub(ended, Ordering::AcqRel);
        }
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
            if st.last == Some(head.hash) {
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
        if known.get(&head.number) == Some(&head.hash) {
            // The head moved back onto a block that is already canonical: a
            // rollback with no new block, so no logs, only the head event.
            fresh.clear();
            ancestor = Some(head.number);
            n = 0;
        }
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
        let mut block_logs: Vec<BlockLogs> = vec![];
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
        let keep_below = ancestor.map_or_else(|| fresh.first().map_or(0, |f| f.0), |a| a + 1);
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
                    if p.overflow {
                        continue;
                    }
                    for (l, v) in logs {
                        if event_matches(f, l, b.number) {
                            if p.logs.len() >= MAX_PENDING_FILTER_ITEMS {
                                p.logs.clear();
                                p.overflow = true;
                                break;
                            }
                            p.logs.push(v.clone());
                        }
                    }
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
            if matches!(p.event, Event::Heads) && !p.overflow {
                if p.hashes.len() >= MAX_PENDING_FILTER_ITEMS {
                    p.hashes.clear();
                    p.overflow = true;
                } else {
                    p.hashes.push(head.hash);
                }
            }
        }
        let mut head_json = None;
        let mut head_v2 = None;
        let head_hash = json::hash_hex(&head.hash);
        for (id, s) in subs.iter_mut() {
            match &s.event {
                Event::Heads => {
                    let v = head_json.get_or_insert_with(|| json::work_object(&head));
                    s.notify(id, v);
                }
                Event::HeadsV2 => {
                    let v = head_v2.get_or_insert_with(|| json::work_object_v2(&head));
                    s.notify(id, v);
                }
                Event::Accesses(address) => {
                    for _ in 0..accesses(&head, address) {
                        s.notify(id, &head_hash);
                    }
                }
                _ => {}
            }
        }
        st.last = Some(head.hash);
        st.handled += 1;
    }

    /// An appended block (`ChainEvent`): "newChainBlocks" and
    /// "newChainBlocksV2" get the block (`RPCMarshalWorkObject`).
    pub fn on_chain_block(&self, hash: Hash) {
        let view = self.view.pinned().unwrap_or_else(|| self.view.clone());
        let Ok(Some(block)) = view.block_or_candidate(&hash) else {
            return;
        };
        let mut st = self.lock();
        let (mut v1, mut v2) = (None, None);
        for (id, s) in st.subs.iter_mut() {
            if let Event::ChainBlocks { v2: version_2 } = s.event {
                let v = if version_2 {
                    v2.get_or_insert_with(|| json::work_object_v2(&block))
                } else {
                    v1.get_or_insert_with(|| json::work_object(&block))
                };
                s.notify(id, v);
            }
        }
    }

    /// The unlocks of an appended block (`UnlocksEvent`): "accesses" gets
    /// the block hash once when its address is among them.
    pub fn on_unlocks(&self, unlocks: &crate::view::Unlocks) {
        let hash = json::hash_hex(&unlocks.block_hash);
        let mut st = self.lock();
        for (id, s) in st.subs.iter_mut() {
            if let Event::Accesses(address) = &s.event {
                if unlocks.addresses.contains(address) {
                    s.notify(id, &hash);
                }
            }
        }
    }
    /// A new pending header (`pendingHeader` subscription).
    pub fn on_pending_header(&self, raw: &[u8]) {
        let val = json::bytes_hex(raw);
        let mut st = self.lock();
        for (id, s) in st.subs.iter_mut() {
            if matches!(s.event, Event::PendingHeader) {
                s.notify(id, &val);
            }
        }
    }
    /// A new block template update (`blockTemplateUpdates` subscription).
    pub fn on_template_update(&self) {
        let reqs: Vec<(String, Option<Value>)> = {
            let st = self.lock();
            st.subs
                .iter()
                .filter_map(|(id, s)| match &s.event {
                    Event::BlockTemplateUpdates(req) => Some((id.clone(), req.clone())),
                    _ => None,
                })
                .collect()
        };
        if reqs.is_empty() {
            return;
        }
        let view = self.view.pinned().unwrap_or_else(|| self.view.clone());
        let mut updates = Vec::new();
        for (id, req) in reqs {
            if let Ok(tpl) = view.get_block_template(req.as_ref()) {
                updates.push((id, tpl));
            }
        }
        let mut st = self.lock();
        for (id, tpl) in updates {
            if let Some(s) = st.subs.get_mut(&id) {
                s.notify(&id, &tpl);
            }
        }
    }
    /// A new workshare (`newWorkshares` and `newWorksharesV2` subscriptions).
    pub fn on_workshare(&self, wo: &quai_primitives::work::WorkObject) {
        let Ok(header) = wo.auxiliary_work_header(quai_consensus::verifier::PINNED_SCHEDULE) else {
            return;
        };
        let hw = header.wire();
        let hash = header.block_hash().unwrap_or_default();
        let num = quai_consensus::share_rules::number(hw.number.as_deref().unwrap_or_default())
            .unwrap_or(0);
        let payload = serde_json::json!({
            "hash": json::hash_hex(&hash),
            "parentHash": json::bytes_hex(hw.parent_hash.as_ref().map(|h| h.value.as_slice()).unwrap_or_default()),
            "number": json::u64_hex(num),
            "difficulty": json::big_hex(hw.difficulty.as_deref().unwrap_or_default()),
            "primaryCoinbase": json::bytes_hex(hw.primary_coinbase.as_ref().map(|c| c.value.as_slice()).unwrap_or_default()),
            "time": json::u64_hex(hw.time.unwrap_or(0)),
            "lock": json::u64_hex(u64::from(hw.lock.unwrap_or(0))),
        });
        let mut st = self.lock();
        for (id, s) in st.subs.iter_mut() {
            if matches!(s.event, Event::Workshares { .. }) {
                s.notify(id, &payload);
            }
        }
    }
    /// The number of head events handled (tests wait on it).
    pub fn handled(&self) -> u64 {
        self.lock().handled
    }
}

/// How many notifications "accesses" sends for a head (the `Accesses`
/// goroutine of quai/filters/api.go): one per transaction whose recipient
/// or sender (Quai), recipient or ETX sender (external) or an output
/// (Qi, at most one per transaction) is the address, and one more per
/// Quai transaction whose access list names it. Go's `break` there leaves
/// only the `switch`, so one transaction can send twice.
fn accesses(head: &BlockData, address: &super::args::Address) -> usize {
    use quai_primitives::transaction::Kind;
    let mut n = 0;
    for tx in head.object.transactions() {
        match tx.kind() {
            Kind::Quai(q) => {
                if q.to.as_ref() == Some(address) || tx.sender().ok().as_ref() == Some(address) {
                    n += 1;
                }
                if q.access_list.iter().any(|t| &t.address == address) {
                    n += 1;
                }
            }
            Kind::External(e) => {
                if &e.to == address || &e.sender == address {
                    n += 1;
                }
            }
            Kind::Qi(q) => {
                let owner = |o: &quai_primitives::transaction::Output| {
                    json::address20(o.address.as_deref().unwrap_or_default())
                };
                if q.outputs.iter().any(|o| &owner(o) == address) {
                    n += 1;
                }
            }
        }
    }
    n
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
