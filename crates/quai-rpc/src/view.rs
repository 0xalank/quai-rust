//! What a node provides to the native JSON-RPC mode (`--mode native`).
//!
//! A `ChainView` holds validated chain data: canonical blocks with their
//! derived fields (order, total entropy, uncle classes), the stored receipts,
//! a transaction index, the account and UTXO state of the blocks whose state
//! it keeps, and a read-only execution entry for `quai_call`,
//! `quai_estimateGas` and `quai_createAccessList`. The native handlers
//! (`native/mod.rs`) turn these values into the JSON that go-quai v0.57.0
//! writes for the same block.
//!
//! The first implementation is `quai_node::rpc_view::ExportView`: a QEX1
//! export plus the state of one replay checkpoint. A live node implements the
//! same trait over its own store, and adds the optional parts at the end of
//! the trait (head events, a transaction pool, the UTXO journals and indexes
//! of the Qi methods), which have defaults here.
use quai_primitives::{
    transaction::{AccessTuple, Address, Transaction},
    work::WorkObject,
    Hash, Header,
};
use std::sync::Arc;

/// A 256-bit unsigned integer, big-endian.
pub type U256 = [u8; 32];

/// How go-quai classifies an uncle of a block
/// (`core.UncleWorkShareClassification`): `RPCMarshalBlock` lists `Block`
/// uncles under "uncles" and `Workshare` uncles under "workshares", and
/// leaves out the others.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UncleKind {
    Block,
    Workshare,
    Other,
}

/// A canonical zone block and the values go-quai derives for it.
#[derive(Clone)]
pub struct BlockData {
    pub hash: Hash,
    /// Zone block number.
    pub number: u64,
    /// The zone work object (`types.BlockObject` view).
    pub object: WorkObject,
    /// Its work-object header.
    pub header: Header,
    /// The uncles in body order (`ChainView::uncle_kinds` classifies them).
    pub uncles: Vec<Header>,
    /// `WorkObject.Size()`: the length of the protobuf block object.
    pub size: u64,
    /// `CalcOrder`: 0 prime, 1 region, 2 zone.
    pub order: u8,
    /// `tx.Hash()` of each transaction, in body order.
    pub tx_hashes: Vec<Hash>,
    /// `tx.Hash()` of each outbound ETX, in body order.
    pub etx_hashes: Vec<Hash>,
}

/// Where a transaction is in the canonical chain (`rawdb.ReadTransaction`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxPosition {
    pub block_hash: Hash,
    pub block_number: u64,
    pub index: u64,
}

/// A log as go-quai stores it (`LogForStorage`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogData {
    pub address: Address,
    pub topics: Vec<Hash>,
    pub data: Vec<u8>,
}

/// A receipt as go-quai stores it (`ReceiptForStorage`). The handlers derive
/// the block, transaction and log positions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptData {
    /// 32 bytes for a pre-Byzantium post state; empty otherwise.
    pub post_state: Vec<u8>,
    pub status: u64,
    pub cumulative_gas_used: u64,
    pub gas_used: u64,
    pub tx_hash: Hash,
    pub contract_address: Option<Address>,
    pub logs: Vec<LogData>,
    pub outbound_etxs: Vec<Transaction>,
}

/// The logs of a canonical block that a reorg removed (go-quai's
/// `RemovedLogsEvent`). go-quai v0.57.0 never sends that event, so the
/// filters and subscriptions do not read them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemovedLogs {
    pub block_hash: Hash,
    pub block_number: u64,
    /// (transaction hash, transaction index, log), in block order.
    pub logs: Vec<(Hash, u64, LogData)>,
}

/// Why the view cannot answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewError {
    /// go-quai would answer with this error message (code -32000).
    Go(String),
    /// The data exists in go-quai but this view does not hold it. The message
    /// says what is missing.
    Unavailable(String),
}
impl std::fmt::Display for ViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Go(m) | Self::Unavailable(m) => f.write_str(m),
        }
    }
}
impl std::error::Error for ViewError {}

/// Account state at one block.
pub trait AccountState: Send + Sync {
    fn balance(&self, address: &Address) -> Result<U256, ViewError>;
    fn nonce(&self, address: &Address) -> Result<u64, ViewError>;
    fn code(&self, address: &Address) -> Result<Vec<u8>, ViewError>;
    fn storage(&self, address: &Address, key: &Hash) -> Result<Hash, ViewError>;
}

/// An unspent output (`types.UtxoEntry`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UtxoData {
    pub denomination: u8,
    pub address: Address,
    /// Big-endian lock height, without leading zeros; empty for no lock.
    pub lock: Vec<u8>,
}

/// An unspent output of one address (`types.OutpointAndDenomination`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutpointData {
    pub tx_hash: Hash,
    pub index: u16,
    pub denomination: u8,
    pub lock: Vec<u8>,
}

/// An output that a block spent or trimmed (`types.SpentUtxoEntry`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpentUtxo {
    pub tx_hash: Hash,
    pub index: u16,
    pub denomination: u8,
    pub address: Address,
    /// Big-endian lock height, without leading zeros; empty for no lock.
    pub lock: Vec<u8>,
}

/// A message call as `TransactionArgs.ToMessage` builds it: gas price zero,
/// the sender's state nonce, no signature. go-quai v0.57.0 cannot decode
/// state overrides (`map[common.AddressBytes]OverrideAccount` has no text
/// key decoder), so a call never has them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallRequest {
    pub from: Address,
    pub to: Option<Address>,
    pub gas: u64,
    pub value: U256,
    pub data: Vec<u8>,
    pub access_list: Vec<AccessTuple>,
    /// Trace the call as one round of go-quai's `AccessList`
    /// (internal/quaiapi/api.go): a `vm.AccessListTracer` seeded with
    /// `access_list`, which leaves out the sender, the recipient (for a
    /// creation the address it creates) and the precompiles unless they have
    /// storage keys. The message runs with `access_list` as given.
    pub trace_access_list: bool,
}

/// `core.ExecutionResult` of a message call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallResult {
    pub used_gas: u64,
    /// The EVM error text as go-quai prints it (`vm.ErrOutOfGas` and so on);
    /// None on success.
    pub error: Option<String>,
    pub reverted: bool,
    pub return_data: Vec<u8>,
    /// With `CallRequest::trace_access_list`: the traced lists.
    pub access_list: Option<TracedAccessList>,
}

/// One round of go-quai's access-list tracing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TracedAccessList {
    /// The tracer's seed: `access_list` after the exclusions.
    pub before: Vec<AccessTuple>,
    /// The tracer's list after the call.
    pub after: Vec<AccessTuple>,
}

/// Why a call did not run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// A consensus error that go-quai returns instead of a result
    /// (`ErrIntrinsicGas`, `ErrInsufficientFunds` and so on), with its text.
    Message { text: String, intrinsic_gas: bool },
    /// The view cannot run this call.
    View(ViewError),
}
impl From<ViewError> for CallError {
    fn from(v: ViewError) -> Self {
        Self::View(v)
    }
}

/// The chain data of one node. Implementations must be cheap to call from
/// several threads at once; the handlers call them on blocking threads.
pub trait ChainView: Send + Sync + 'static {
    /// Chain ID (9 on mainnet).
    fn chain_id(&self) -> u64;
    /// The zone this view serves.
    fn location(&self) -> [u8; 2];
    /// The number of the head block: the newest block with validated state.
    fn head(&self) -> u64;
    fn block_by_number(&self, number: u64) -> Result<Option<Arc<BlockData>>, ViewError>;
    fn block_by_hash(&self, hash: &Hash) -> Result<Option<Arc<BlockData>>, ViewError>;
    /// The class of each uncle, in body order. This needs the uncles'
    /// proof-of-work, so only the block JSON asks for it.
    fn uncle_kinds(&self, block: &BlockData) -> Result<Arc<Vec<UncleKind>>, ViewError>;
    /// `TotalLogEntropy` of the block in the zone context.
    fn total_entropy(&self, block: &BlockData) -> Result<U256, ViewError>;
    /// The canonical position of a block transaction (not of outbound ETXs).
    fn transaction(&self, hash: &Hash) -> Result<Option<TxPosition>, ViewError>;
    /// The stored receipts of a canonical block, in transaction order.
    /// `Err(Unavailable)` when the view has the block but not its receipts.
    fn receipts(&self, block: &BlockData) -> Result<Arc<Vec<ReceiptData>>, ViewError>;
    /// Whether the block at `number` can have a log (lets `quai_getLogs`
    /// skip blocks without loading them). `true` when unsure.
    fn may_have_logs(&self, _number: u64) -> Result<bool, ViewError> {
        Ok(true)
    }
    /// The account state after `block`. `Err(Go(..))` carries go-quai's
    /// answer when that state is not kept ("missing trie node ...").
    fn state(&self, block: &BlockData) -> Result<Arc<dyn AccountState>, ViewError>;
    /// The UTXO set at the head.
    fn utxo(&self, tx_hash: &Hash, index: u16) -> Result<Option<UtxoData>, ViewError>;
    /// The unspent outputs of an address at the head.
    fn outpoints_by_address(&self, address: &Address) -> Result<Vec<OutpointData>, ViewError>;
    /// The unspent outputs that blocks `start..=end` created for addresses
    /// with the first 16 bytes of `address`
    /// (`core.GetOutpointsByAddressAndRange`).
    fn outpoints_by_address_and_range(
        &self,
        address: &Address,
        start: u32,
        end: u32,
    ) -> Result<Vec<OutpointData>, ViewError>;
    /// Run a message on the state after `block`, as `DoCall` does: with
    /// go-quai's `vm.Config{Debug: true}`, so access-list checks are off
    /// and every account and slot is warm. Discard every change.
    fn call(&self, block: &BlockData, request: &CallRequest) -> Result<CallResult, CallError>;
    /// go-quai `HeaderChain.CalcBaseFee(block)`: the minimum base fee of the
    /// block after `block`, which `quai_gasPrice` raises by 20%. The default
    /// computes it from the block and its prime terminus
    /// (`native::fees::calc_base_fee`).
    fn calc_base_fee(&self, block: &BlockData) -> Result<U256, ViewError> {
        crate::native::fees::calc_base_fee(self, block)
    }

    /// The view one call reads. A view whose head moves (a running node)
    /// returns a snapshot of its current head, so that every read of the
    /// call sees the same head, blocks and state. None: read this view.
    fn pinned(&self) -> Option<Arc<dyn ChainView>> {
        None
    }
    /// New canonical heads, for filters and subscriptions
    /// (`native::filters`): each hash wakes the event loop, which sends the
    /// logs of every block that became canonical and then the head. A view
    /// without a live head has none.
    fn head_events(&self) -> Option<tokio::sync::broadcast::Receiver<Hash>> {
        None
    }
    /// Logs that reorgs removed from the canonical chain (unused: go-quai
    /// sends none). A view without a live head has none.
    fn removed_logs(&self) -> Option<tokio::sync::broadcast::Receiver<Arc<RemovedLogs>>> {
        None
    }
    /// `quai_sendRawTransaction` and `quai_receiveTxFromPoolSharingClient`:
    /// decode the protobuf transaction and submit it as go-quai's
    /// `SubmitTransaction` does. The hash, or `ViewError::Go` with go-quai's
    /// error text. A view without a transaction pool refuses.
    fn submit_transaction(&self, _raw: &[u8]) -> Result<Hash, ViewError> {
        Err(ViewError::Unavailable(
            "this node has no transaction pool".into(),
        ))
    }
    /// `TxPool.Nonce`: the next nonce with the pool's pending transactions
    /// applied. None: no pool (the head state answers).
    fn pool_nonce(&self, _address: &Address) -> Option<u64> {
        None
    }
    /// `TxPool.Get`: a pending, queued or Qi pool transaction.
    fn pool_transaction(&self, _hash: &Hash) -> Option<PoolTransaction> {
        None
    }
    /// `TxPool.Content`, or `ContentFrom` an address. None: no pool.
    fn pool_content(&self, _from: Option<&Address>) -> Option<PoolContent> {
        None
    }
    /// `TxPool.Stats`: pending, queued and Qi transactions. None: no pool.
    fn pool_stats(&self) -> Option<(u64, u64, u64)> {
        None
    }

    /// The canonical hash at `number` (the filter system walks the chain
    /// with it). The default loads the block.
    fn canonical_hash(&self, number: u64) -> Result<Option<Hash>, ViewError> {
        Ok(self.block_by_number(number)?.map(|b| b.hash))
    }
    /// `BlockOrCandidateByHash` and `HeaderByHash`: a block by hash,
    /// canonical or not. The default knows the canonical chain only.
    fn block_or_candidate(&self, hash: &Hash) -> Result<Option<Arc<BlockData>>, ViewError> {
        self.block_by_hash(hash)
    }
    /// `rawdb.ReadUTXOSetSize` of a block (`quai_estimateFeeForQi` reads
    /// the head's).
    fn utxo_set_size(&self, _block: &BlockData) -> Result<u64, ViewError> {
        Err(ViewError::Unavailable("the UTXO set size".into()))
    }
    /// The outputs a block removed: `rawdb.ReadSpentUTXOs` (the inputs of
    /// its Qi transactions, in order) followed by `rawdb.ReadTrimmedUTXOs`.
    fn spent_utxos(&self, _block: &BlockData) -> Result<Vec<SpentUtxo>, ViewError> {
        Err(ViewError::Unavailable(
            "the spent outputs of a block".into(),
        ))
    }
    /// `rawdb.ReadLockedBalance`: the Quai of coinbase lockups and Qi-to-Quai
    /// conversions of an address that are still locked, as go-quai keeps it
    /// with `--node.index-address-utxos` (core/state_processor.go
    /// `WriteNewLockups`). The chain processor must supply it.
    fn locked_quai(&self, _address: &Address) -> Result<U256, ViewError> {
        Err(ViewError::Unavailable("the locked Quai index".into()))
    }
    /// `GetSlicesRunning`: the zones this node runs.
    fn running_chains(&self) -> Vec<[u8; 2]> {
        vec![self.location()]
    }
}

/// A transaction of the pool and its go-quai hash (`tx.Hash()`).
#[derive(Clone, Debug)]
pub struct PoolTransaction {
    pub hash: Hash,
    pub tx: Transaction,
}

/// The pool's Quai transactions by account, each list in nonce order.
#[derive(Clone, Debug, Default)]
pub struct PoolContent {
    pub pending: Vec<(Address, Vec<PoolTransaction>)>,
    pub queued: Vec<(Address, Vec<PoolTransaction>)>,
}
