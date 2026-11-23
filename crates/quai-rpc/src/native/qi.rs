//! The Qi and conversion methods of go-quai v0.57.0
//! (internal/quaiapi/quai_api.go): `quai_qiToQuai`, `quai_quaiToQi`,
//! `quai_calculateConversionAmount`, `quai_estimateFeeForQi`,
//! `quai_getOutpointDeltasForAddressesInRange`, `quai_getLockedBalance`,
//! `quai_getWrappedQiDeposit` and `quai_listRunningChains`.
//!
//! The quotes follow consensus/misc/rewards.go with `big.Int` semantics:
//! `QiToQuai` and `QuaiToQi` scale by `CalculateQuaiConversionReward` over
//! `CalculateQiReward` of one block. After KawPow a difficulty below
//! `KQuaiDifficultyDivisor` (3e11) makes the Quai reward negative, and Go
//! answers negative amounts ("-0x..."); this module keeps the sign.
//! `quai_consensus::economics::equivalent_difficulty` gives the KawPow
//! equivalent difficulty and `cubic_discount` `misc.ApplyCubicDiscount`.
//! Where Go panics (a zero difficulty in `BinaryLog`, a nil block, an index
//! out of range) the answer is go-quai's "method handler crashed".
use super::args::{self, Args, BlockNumber, BlockRef};
use super::{call_args, head_block, internal, json, qi, RpcError};
use crate::view::{BlockData, ChainView};
use crypto_bigint::{Encoding, U1024};
use quai_consensus::{cubic_discount, economics, rules::Uint, share_rules};
use quai_execution::evm::{state_gas, word::from_bytes};
use quai_primitives::{
    spec::{self, FORKS},
    Hash, Header,
};
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;

/// Maximum number of addresses accepted in one
/// `quai_getOutpointDeltasForAddressesInRange` call.
const MAX_OUTPOINT_DELTA_ADDRESSES: usize = 256;
/// Maximum parent steps walked by `common_ancestor`.
const MAX_COMMON_ANCESTOR_STEPS: usize = 2048;

type R<T> = Result<T, RpcError>;
type Address = [u8; 20];

/// The methods of this module.
pub const METHODS: &[&str] = &[
    "quai_qiToQuai",
    "quai_quaiToQi",
    "quai_calculateConversionAmount",
    "quai_estimateFeeForQi",
    "quai_getOutpointDeltasForAddressesInRange",
    "quai_getLockedBalance",
    "quai_getWrappedQiDeposit",
    "quai_listRunningChains",
    "quai_baseFee",
    "quai_suggestFinalityDepth",
    "quai_hashesPerQits",
];

fn crashed() -> RpcError {
    RpcError::go("method handler crashed")
}

pub fn dispatch(view: &dyn ChainView, method: &str, params: &Value) -> Option<R<Value>> {
    dispatch_cancellable(view, method, params, None)
}
pub fn dispatch_cancellable(
    view: &dyn ChainView,
    method: &str,
    params: &Value,
    cancel: Option<&AtomicBool>,
) -> Option<R<Value>> {
    Some(match method {
        "quai_qiToQuai" => quote_rpc(view, params, true),
        "quai_quaiToQi" => quote_rpc(view, params, false),
        "quai_calculateConversionAmount" => conversion_amount(view, params),
        "quai_estimateFeeForQi" => estimate_fee_for_qi(view, params),
        "quai_getOutpointDeltasForAddressesInRange" => outpoint_deltas(view, params, cancel),
        "quai_getLockedBalance" => locked_balance(view, params),
        "quai_getWrappedQiDeposit" => wrapped_qi_deposit(view, params),
        "quai_baseFee" => base_fee(view, params),
        "quai_suggestFinalityDepth" => finality_depth(view, params),
        "quai_hashesPerQits" => hashes_per_qits(view, params),
        "quai_listRunningChains" => Args::new(params, 0).map(|_| {
            Value::Array(
                view.running_chains()
                    .iter()
                    .map(|[r, z]| Value::Array(vec![Value::from(*r), Value::from(*z)]))
                    .collect(),
            )
        }),
        _ => return None,
    })
}

// --- big.Int arithmetic -----------------------------------------------------

fn wide(bytes: &[u8]) -> U1024 {
    let mut b = [0u8; 128];
    let take = bytes.len().min(128);
    b[128 - take..].copy_from_slice(&bytes[bytes.len() - take..]);
    U1024::from_be_bytes(b)
}
fn from_uint(v: Uint) -> U1024 {
    v.resize()
}
fn magnitude(v: U1024) -> Vec<u8> {
    let b = v.to_be_bytes();
    b[b.iter().position(|x| *x != 0).unwrap_or(b.len())..].to_vec()
}
fn uint256(v: U1024) -> Option<Uint> {
    (v.bits_vartime() <= 256).then(|| v.resize())
}
fn field(v: &Option<Vec<u8>>) -> Uint {
    share_rules::uint(v.as_deref().unwrap_or_default()).unwrap_or(Uint::ZERO)
}

/// A signed integer with `big.Int`'s operations; the magnitude stays far
/// below 1,024 bits (256-bit amounts times quotes of about 340 bits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Int {
    neg: bool,
    mag: U1024,
}
impl Int {
    fn pos(mag: U1024) -> Self {
        Self { neg: false, mag }
    }
    fn of(v: u64) -> Self {
        Self::pos(U1024::from_u64(v))
    }
    fn signed(neg: bool, mag: U1024) -> Self {
        Self {
            neg: neg && mag != U1024::ZERO,
            mag,
        }
    }
    fn is_zero(self) -> bool {
        self.mag == U1024::ZERO
    }
    fn mul(self, o: Self) -> Self {
        Self::signed(self.neg != o.neg, self.mag.wrapping_mul(&o.mag))
    }
    /// `Quo`: truncated toward zero; None for a zero divisor (Go panics).
    fn quo(self, o: Self) -> Option<Self> {
        if o.is_zero() {
            return None;
        }
        Some(Self::signed(
            self.neg != o.neg,
            self.mag.wrapping_div(&o.mag),
        ))
    }
    /// `Div` by a positive divisor: Euclidean, so a negative quotient
    /// rounds down.
    fn div_floor(self, d: u64) -> Self {
        let d = U1024::from_u64(d);
        let q = self.mag.wrapping_div(&d);
        if self.neg && q.wrapping_mul(&d) != self.mag {
            Self::signed(true, q.wrapping_add(&U1024::ONE))
        } else {
            Self::signed(self.neg, q)
        }
    }
    fn add(self, o: Self) -> Self {
        if self.neg == o.neg {
            return Self::signed(self.neg, self.mag.wrapping_add(&o.mag));
        }
        if self.mag >= o.mag {
            Self::signed(self.neg, self.mag.wrapping_sub(&o.mag))
        } else {
            Self::signed(o.neg, o.mag.wrapping_sub(&self.mag))
        }
    }
    fn json(self) -> Value {
        let v = json::big_hex(&magnitude(self.mag));
        match (self.neg, v) {
            (true, Value::String(s)) => Value::String(format!("-{s}")),
            (_, v) => v,
        }
    }
}
impl PartialOrd for Int {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Int {
    fn cmp(&self, o: &Self) -> Ordering {
        match (self.neg, o.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => self.mag.cmp(&o.mag),
            (true, true) => o.mag.cmp(&self.mag),
        }
    }
}

// --- consensus/misc/rewards.go ------------------------------------------------

fn prime_of(header: &Header) -> Option<u64> {
    share_rules::number(header.wire().prime_terminus_number.as_deref()?).ok()
}
/// `ForkAwareKawPowEquivalentDifficulty` from the KawPow fork, else the
/// difficulty.
fn equivalent(header: &Header, difficulty: Uint) -> Option<Uint> {
    prime_of(header)?;
    economics::equivalent_difficulty(header, difficulty).ok()
}
/// `CalculateQuaiReward`: `rate * logDiff / 2^64`, at least 1 when zero;
/// negative when the reset logarithm exceeds the difficulty's.
fn quai_reward(header: &Header, difficulty: Uint, rate: Uint) -> Option<Int> {
    let prime = prime_of(header)?;
    let d = equivalent(header, difficulty)?;
    if d == Uint::ZERO {
        return None;
    }
    let mut log = state_gas::log_q64(d) as i128;
    if prime >= FORKS.kawpow {
        log -= state_gas::log_q64(Uint::from_u64(300_000_000_000)) as i128;
    }
    let log = Int::signed(log < 0, U1024::from_u128(log.unsigned_abs()));
    let reward = Int::pos(from_uint(rate))
        .mul(log)
        .quo(Int::pos(U1024::ONE.shl_vartime(64)))?;
    Some(if reward.is_zero() { Int::of(1) } else { reward })
}
/// `CalculateQiReward`: the difficulty over `OneOverKqi`, at least 1.
fn qi_reward(header: &Header, difficulty: Uint) -> Option<Int> {
    let d = equivalent(header, difficulty)?;
    let number = share_rules::number(header.wire().number.as_deref()?).ok()?;
    let q = from_uint(d).wrapping_div(&from_uint(economics::one_over_kqi(number)));
    Some(if q == U1024::ZERO {
        Int::of(1)
    } else {
        Int::pos(q)
    })
}
/// `CalculateQuaiConversionReward`: with twice the average fees from the
/// ConversionLockChange fork.
fn conversion_reward(block: &BlockData, rate: Uint, difficulty: Uint) -> Option<Int> {
    let reward = quai_reward(&block.header, difficulty, rate)?;
    if prime_of(&block.header)? < FORKS.conversion_lock_change {
        return Some(reward);
    }
    let fees = from_uint(field(&block.object.header().wire().avg_tx_fees));
    Some(reward.add(Int::pos(fees.wrapping_mul(&U1024::from_u64(2)))))
}
/// `misc.QiToQuai`.
fn qi_to_quai(block: &BlockData, rate: Uint, difficulty: Uint, amount: Int) -> Option<Int> {
    let quai = conversion_reward(block, rate, difficulty)?;
    let qi = qi_reward(&block.header, difficulty)?;
    quai.mul(amount).quo(qi)
}
/// `misc.QuaiToQi`.
fn quai_to_qi(block: &BlockData, rate: Uint, difficulty: Uint, amount: Int) -> Option<Int> {
    let quai = conversion_reward(block, rate, difficulty)?;
    let qi = qi_reward(&block.header, difficulty)?;
    qi.mul(amount).quo(quai)
}
/// `misc.ApplyCubicDiscount(value, mean).Int(nil)`. A negative value is
/// below the mean and takes the 20-basis-point branch, which is symmetric.
fn cubic(value: Int, mean: Uint) -> Option<Int> {
    let mag = uint256(value.mag)?;
    if value.neg {
        let d = cubic_discount::apply(mag, mag).ok()?;
        return Some(Int::signed(true, from_uint(d)));
    }
    Some(Int::pos(from_uint(cubic_discount::apply(mag, mean).ok()?)))
}

fn prime_hash(block: &BlockData) -> Hash {
    block
        .object
        .header()
        .wire()
        .prime_terminus_hash
        .as_ref()
        .and_then(|h| h.value.as_slice().try_into().ok())
        .unwrap_or_default()
}
/// `GetBlockByHash(block.PrimeTerminusHash())`.
fn prime_terminus(view: &dyn ChainView, block: &BlockData) -> R<Option<Arc<BlockData>>> {
    Ok(view.block_or_candidate(&prime_hash(block))?)
}
fn rate(block: &BlockData) -> Uint {
    field(&block.object.header().wire().exchange_rate)
}
fn miner_difficulty(block: &BlockData) -> Uint {
    field(&block.object.header().wire().miner_difficulty)
}
fn difficulty(block: &BlockData) -> Uint {
    share_rules::uint(
        block
            .header
            .wire()
            .difficulty
            .as_deref()
            .unwrap_or_default(),
    )
    .unwrap_or(Uint::ZERO)
}
/// `NumberU64(PRIME_CTX)`: the prime number of the body header.
fn prime_number(block: &BlockData) -> u64 {
    let n = block
        .object
        .header()
        .wire()
        .number
        .first()
        .cloned()
        .unwrap_or_default();
    let n = &n[n.len().saturating_sub(8)..];
    let mut b = [0u8; 8];
    b[8 - n.len()..].copy_from_slice(n);
    u64::from_be_bytes(b)
}

// --- quotes -----------------------------------------------------------------

/// `QiToQuai` (`qi` true) and `QuaiToQi`: an amount at a block. Both answer
/// null for a block or prime terminus they cannot find, and `QuaiToQi`
/// prices with the prime terminus of the current block whatever block is
/// asked.
fn quote_rpc(view: &dyn ChainView, params: &Value, qi: bool) -> R<Value> {
    let a = Args::new(params, 2)?;
    let amount = a.get(0, args::big)?;
    let at = a.get(1, args::block_ref)?;
    let header = match at {
        BlockRef::Number(BlockNumber::Latest) => Some(head_block(view)?),
        // `PendingBlock()`: no miner.
        BlockRef::Number(BlockNumber::Pending) => None,
        BlockRef::Number(BlockNumber::Number(n)) if n > view.head() => None,
        BlockRef::Number(BlockNumber::Number(n)) => view.block_by_number(n)?,
        BlockRef::Hash { hash, .. } => view.block_or_candidate(&hash)?,
        BlockRef::Neither => None,
    };
    let Some(header) = header else {
        return Ok(Value::Null);
    };
    let terminus_of = if qi {
        header.clone()
    } else {
        head_block(view)?
    };
    let Some(prime) = prime_terminus(view, &terminus_of)? else {
        return Ok(Value::Null);
    };
    let amount = Int::pos(wide(&amount));
    let out = if qi {
        qi_to_quai(&header, rate(&prime), miner_difficulty(&prime), amount)
    } else {
        quai_to_qi(&header, rate(&prime), miner_difficulty(&prime), amount)
    };
    Ok(out.ok_or_else(crashed)?.json())
}

/// `CalculateConversionAmount`: the value a conversion of `tx` delivers at
/// the current prime terminus.
fn conversion_amount(view: &dyn ChainView, params: &Value) -> R<Value> {
    let a = Args::new(params, 1)?;
    let tx = a.get(0, call_args)?;
    let from = tx
        .from
        .ok_or_else(|| RpcError::go("from field cannot be nil"))?;
    let to = tx
        .to
        .ok_or_else(|| RpcError::go("to field cannot be nil"))?;
    let value = tx
        .value
        .ok_or_else(|| RpcError::go("cannot convert nil or empty value"))?;
    if qi(&from) == qi(&to) {
        return Err(RpcError::go(
            "from and to cannot be in the same ledger for conversion",
        ));
    }
    let current = head_block(view)?;
    let prime = prime_terminus(view, &current)?
        .ok_or_else(|| RpcError::go("cannot find the prime terminus of the current header"))?;
    let rate = rate(&prime);
    let miner = miner_difficulty(&prime);
    let mut value = Int::pos(wide(&value));
    let ten_percent = value.mul(Int::of(10)).div_floor(100);
    let from_qi = qi(&from);
    let quai_amount = if from_qi {
        qi_to_quai(&prime, rate, miner, value).ok_or_else(crashed)?
    } else {
        value
    };
    let flow = field(&prime.object.header().wire().conversion_flow_amount);
    let after_cubic = cubic(quai_amount, flow).ok_or_else(crashed)?;
    value = if from_qi {
        quai_to_qi(&prime, rate, miner, after_cubic).ok_or_else(crashed)?
    } else {
        after_cubic
    };
    // value * (KQuaiDiscountMultiplier - kQuaiDiscount) / KQuaiDiscountMultiplier
    let k = Int::pos(from_uint(field(
        &prime.object.header().wire().k_quai_discount,
    )));
    let factor = Int::of(100_000).add(Int::signed(!k.neg, k.mag));
    let after_k = value.mul(factor).div_floor(100_000);
    let mut increasing = false;
    let number = prime_number(&prime);
    if number > 4000 {
        // A zone block at a prime number: go-quai's `BlockByNumber`; a
        // missing block is (nil, nil), and its rate read panics.
        let previous = if number - 4000 > view.head() {
            None
        } else {
            view.block_by_number(number - 4000)?
        };
        let previous = previous.ok_or_else(crashed)?;
        increasing = from_uint(rate) > from_uint(self::rate(&previous));
    }
    let discount = if from_qi { !increasing } else { increasing };
    if discount && !value.is_zero() {
        value = after_k;
    }
    if value < ten_percent {
        value = ten_percent;
    }
    let out = if from_qi {
        qi_to_quai(&prime, rate, miner, value)
    } else {
        quai_to_qi(&prime, rate, miner, value)
    };
    Ok(out.ok_or_else(crashed)?.json())
}

// --- Qi fee -------------------------------------------------------------------

/// The Qi fields of `TransactionArgs` (`txIn`, `txOut`, `txType`).
struct QiArgs {
    tx_type: u8,
    /// `previousOutPoint.index` of each input.
    inputs: Vec<u64>,
    /// The address of each output.
    outputs: Vec<Address>,
}
fn in_qi_field<T>(r: Result<T, String>, field: &str) -> Result<T, String> {
    r.map_err(|e| {
        e.replace(
            "into Go value of type",
            &format!("into Go struct field {field} of type"),
        )
    })
}
fn qi_args(v: &Value) -> Result<QiArgs, String> {
    use super::member;
    let base = call_args(v)?;
    let empty = Map::new();
    let m = v.as_object().unwrap_or(&empty);
    let get = |m: &'_ Map<String, Value>, k: &str| -> Option<Value> {
        member(m, k).filter(|x| !x.is_null()).cloned()
    };
    let list = |k: &str, typ: &str| -> Result<Vec<Value>, String> {
        match get(m, k) {
            None => Ok(vec![]),
            Some(Value::Array(items)) => Ok(items),
            Some(other) => Err(format!(
                "json: cannot unmarshal {} into Go struct field TransactionArgs.{k} of type {typ}",
                kind(&other)
            )),
        }
    };
    let mut inputs = vec![];
    for i in list("txIn", "[]types.RPCTxIn")? {
        let i = i.as_object().cloned().unwrap_or_default();
        let point = get(&i, "previousOutPoint")
            .and_then(|p| p.as_object().cloned())
            .unwrap_or_default();
        let index = match get(&point, "index") {
            Some(x) => in_qi_field(args::uint64(&x), "OutpointJSON.txIn.previousOutPoint.index")?,
            None => 0,
        };
        if let Some(h) = get(&point, "txHash") {
            in_qi_field(args::hash(&h), "OutpointJSON.txIn.previousOutPoint.txHash")?;
        }
        if let Some(k) = get(&i, "pubKey") {
            in_qi_field(args::bytes(&k), "RPCTxIn.txIn.pubKey")?;
        }
        inputs.push(index);
    }
    let mut outputs = vec![];
    for o in list("txOut", "[]types.RPCTxOut")? {
        let o = o.as_object().cloned().unwrap_or_default();
        if let Some(d) = get(&o, "denomination") {
            in_qi_field(args::uint(&d), "RPCTxOut.txOut.denomination")?;
        }
        if let Some(l) = get(&o, "lock") {
            in_qi_field(args::big(&l), "RPCTxOut.txOut.lock")?;
        }
        let address = match get(&o, "address") {
            Some(a) => in_qi_field(args::address(&a), "RPCTxOut.txOut.address")?,
            None => [0; 20],
        };
        outputs.push(address);
    }
    Ok(QiArgs {
        tx_type: base.tx_type,
        inputs,
        outputs,
    })
}
fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
/// `TransactionArgs.CalculateQiTxGas`: `types.CalculateQiTxGas` of the
/// inputs and outputs.
fn qi_tx_gas(a: &QiArgs, scaling: f64, location: [u8; 2]) -> R<u64> {
    if a.tx_type != 2 {
        return Err(RpcError::go("not a Qi transaction"));
    }
    if a.inputs.is_empty() || a.outputs.is_empty() {
        return Err(RpcError::go(
            "Qi transaction must have at least one input and one output",
        ));
    }
    if a.inputs.len() > u16::MAX as usize {
        return Err(RpcError::go(format!(
            "Qi transaction has too many inputs: {}",
            a.inputs.len()
        )));
    }
    if a.outputs.len() > u16::MAX as usize {
        return Err(RpcError::go(format!(
            "Qi transaction has too many outputs: {}",
            a.outputs.len()
        )));
    }
    if let Some(i) = a.inputs.iter().find(|i| **i > u16::MAX as u64) {
        return Err(RpcError::go(format!(
            "Qi transaction has an input with an index too large: {i}"
        )));
    }
    // CalculateIntrinsicQiTxGas: SloadGas per input, CallValueTransferGas
    // per output, EcrecoverGas, scaled by the UTXO set size.
    let base = (a.inputs.len() as u64)
        .wrapping_mul(800)
        .wrapping_add((a.outputs.len() as u64).wrapping_mul(9000))
        .wrapping_add(3000);
    let mut gas = if scaling < 15.0 || scaling.is_nan() {
        base
    } else {
        let scaled = scaling * base as f64;
        (if scaled >= 18_446_744_073_709_551_616.0 {
            1u64 << 63
        } else {
            scaled as u64
        }) / 15
    };
    let prefix = location[0] << 4 | location[1];
    let one = from_bytes(&[1]);
    let zero = from_bytes(&[]);
    let conversion = 21000
        + 21000
        + 2 * state_gas::scaled(one, zero, 2100)
        + 2 * state_gas::scaled(one, zero, 20000);
    for o in &a.outputs {
        if o[0] != prefix {
            // An output to another chain creates an ETX.
            gas = gas.wrapping_add(21000 + 21000);
        } else if o[1] <= 127 {
            // An output to this zone's Quai ledger is a conversion.
            gas = gas.wrapping_add(conversion);
        }
    }
    Ok(gas)
}
/// `EstimateFeeForQi`: the Qi fee of a transaction at the current block.
fn estimate_fee_for_qi(view: &dyn ChainView, params: &Value) -> R<Value> {
    let a = Args::new(params, 1)?;
    let args = a.get(0, qi_args)?;
    let head = head_block(view)?;
    let size = view.utxo_set_size(&head)?;
    let gas = qi_tx_gas(&args, (size as f64).ln(), view.location())?;
    let base_fee = wide(
        head.object
            .header()
            .wire()
            .base_fee
            .as_deref()
            .unwrap_or_default(),
    );
    let base_fee = base_fee
        .wrapping_mul(&U1024::from_u64(120))
        .wrapping_div(&U1024::from_u64(100));
    let fee_quai = Int::pos(U1024::from_u64(gas).wrapping_mul(&base_fee));
    let prime = prime_terminus(view, &head)?
        .ok_or_else(|| RpcError::go("cannot find prime terminus for the current block"))?;
    let mut fee_qi = qi_fee_estimate(&head, rate(&prime), fee_quai)?;
    if fee_qi.is_zero() {
        fee_qi = Int::of(1);
    }
    Ok(fee_qi.json())
}
/// `quaiToQiFeeEstimate`: the Qi fee, rounded up when it would buy less
/// Quai than `fee_quai`.
fn qi_fee_estimate(head: &BlockData, rate: Uint, fee_quai: Int) -> R<Int> {
    let diff = difficulty(head);
    let fee_qi = quai_to_qi(head, rate, diff, fee_quai).ok_or_else(crashed)?;
    if qi_to_quai(head, rate, diff, fee_qi).ok_or_else(crashed)? < fee_quai {
        return Ok(fee_qi.add(Int::of(1)));
    }
    Ok(fee_qi)
}

/// `BaseFee`: the head's base fee in Quai (`txType` true), or in qits at
/// its prime terminus's rate, at least one qit.
fn base_fee(view: &dyn ChainView, params: &Value) -> R<Value> {
    let a = Args::new(params, 1)?;
    let quai = a.get(0, args::boolean)?;
    let head = head_block(view)?;
    let Some(fee) = head.object.header().wire().base_fee.clone() else {
        // A nil base fee: null, and QuaiToQi panics on it.
        return if quai {
            Ok(Value::Null)
        } else {
            Err(crashed())
        };
    };
    if quai {
        return Ok(json::big_hex(&fee));
    }
    let prime =
        prime_terminus(view, &head)?.ok_or_else(|| RpcError::go("prime terminus not found"))?;
    let fee_qi = qi_fee_estimate(&head, rate(&prime), Int::pos(wide(&fee)))?;
    Ok(if fee_qi.is_zero() { Int::of(1) } else { fee_qi }.json())
}

/// `SuggestFinalityDepth`: `qiValue * correlatedRisk / CalculateQiReward`
/// of the head. Go converts both arguments to `int64` and divides
/// Euclidean; `Uint64()` keeps the low 64 bits of the magnitude.
fn finality_depth(view: &dyn ChainView, params: &Value) -> R<Value> {
    let a = Args::new(params, 2)?;
    let value = a.get(0, args::uint64)? as i64;
    let risk = a.get(1, args::uint64)? as i64;
    let head = head_block(view)?;
    let reward = qi_reward(&head.header, difficulty(&head)).ok_or_else(crashed)?;
    let product = value as i128 * risk as i128;
    let mag = U1024::from_u128(product.unsigned_abs());
    let mut q = mag.wrapping_div(&reward.mag);
    if product < 0 && q.wrapping_mul(&reward.mag) != mag {
        q = q.wrapping_add(&U1024::ONE);
    }
    let b = q.to_be_bytes();
    let mut low = [0u8; 8];
    low.copy_from_slice(&b[b.len() - 8..]);
    Ok(json::u64_hex(u64::from_be_bytes(low)))
}

/// `HashesPerQits`: the amount times `params.OneOverKqi` of a block number
/// ("latest" is the head; "pending" is `uint64(-2)`); a hash is an error.
fn hashes_per_qits(view: &dyn ChainView, params: &Value) -> R<Value> {
    let a = Args::new(params, 2)?;
    let amount = a.get(0, args::big)?;
    let at = a.get(1, args::block_ref)?;
    if amount == [0; 32] {
        return Ok(json::big_hex(&[]));
    }
    let number = match at {
        BlockRef::Number(BlockNumber::Latest) => view.head(),
        BlockRef::Number(n) => n.int64() as u64,
        _ => return Err(RpcError::go("invalid block number or hash")),
    };
    let total = wide(&amount).wrapping_mul(&from_uint(economics::one_over_kqi(number)));
    Ok(json::big_hex(&magnitude(total)))
}

// --- balances ---------------------------------------------------------------

/// `types.Denominations` in qits.
const DENOMINATIONS: [u64; 15] = [
    1, 5, 10, 50, 100, 500, 1000, 5000, 10000, 20000, 100000, 1000000, 10000000, 100000000,
    1000000000,
];
fn lock_height(lock: &[u8]) -> u128 {
    lock.iter().fold(0u128, |a, b| {
        a.saturating_mul(256).saturating_add(*b as u128)
    })
}
/// `GetLockedBalance`: the locked Quai index of a Quai address, or the
/// locked outputs of a Qi address.
fn locked_balance(view: &dyn ChainView, params: &Value) -> R<Value> {
    let a = Args::new(params, 1)?;
    let (address, _) = a.get(0, args::mixed_address)?;
    if internal(view, &address) && !qi(&address) {
        return Ok(json::big_hex(&view.locked_quai(&address)?));
    }
    if internal(view, &address) && qi(&address) {
        let outs = view.outpoints_by_address(&address)?;
        let head = view.head() as u128;
        let total: u128 = outs
            .iter()
            .filter(|o| !o.lock.is_empty() && head < lock_height(&o.lock))
            .map(|o| *DENOMINATIONS.get(o.denomination as usize).unwrap_or(&0) as u128)
            .sum();
        return Ok(json::big_hex(&total.to_be_bytes()));
    }
    Err(RpcError::go(format!(
        "address {} is invalid",
        json::checksum(&address)
    )))
}

/// `GetWrappedQiDeposit`: the wrapped-Qi deposit of an owner contract and a
/// miner in the lockup contract's storage.
fn wrapped_qi_deposit(view: &dyn ChainView, params: &Value) -> R<Value> {
    let a = Args::new(params, 3)?;
    let owner = a.get(0, args::address)?;
    let miner = a.get(1, args::address)?;
    let at = a.get(2, args::block_ref)?;
    super::quai_internal(view, &owner)?;
    if !internal(view, &miner) {
        return Err(RpcError::go("Address belongs to other zone"));
    }
    let Some((_, state)) = super::state_at(view, at)? else {
        return Ok(Value::Null);
    };
    // vm.GetWrappedQiDeposit checks the miner as a Quai address too.
    super::quai_internal(view, &miner)?;
    let [r, z] = view.location();
    let mut contract = [0u8; 20];
    contract[0] = r << 4 | z;
    contract[19] = 0x0a;
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(&owner[..16]);
    key[16..].copy_from_slice(&miner[..16]);
    let value = state.storage(&contract, &key)?;
    if value == [0; 32] {
        return Err(RpcError::go("no wrapped Qi balance"));
    }
    Ok(json::big_hex(&value))
}

// --- outpoint deltas ----------------------------------------------------------

/// `[]common.Address`.
fn address_list(v: &Value) -> Result<Vec<Address>, String> {
    match v {
        Value::Null => Ok(vec![]),
        Value::Array(items) => items.iter().map(args::address).collect(),
        other => Err(args::type_error(kind(other), "[]common.Address")),
    }
}

type Deltas = BTreeMap<String, [BTreeMap<String, Vec<Value>>; 2]>;
fn output(index: u64, denomination: u8, lock: &[u8]) -> Value {
    json!({
        "index": json::u64_hex(index),
        "denomination": json::u64_hex(denomination as u64),
        "lock": json::big_hex(lock),
    })
}
fn add(out: &mut Deltas, address: &Address, created: bool, tx: &Hash, v: Value) {
    if let Some(entry) = out.get_mut(&json::checksum(address)) {
        entry[usize::from(!created)]
            .entry(quai_primitives::encode_hex(tx))
            .or_default()
            .push(v);
    }
}
/// `misc.FindMinDenominations`: the greedy count of each denomination.
fn min_denominations(value: U1024) -> [u64; 15] {
    let mut counts = [0u64; 15];
    let mut left = value;
    for d in (0..15).rev() {
        let unit = U1024::from_u64(DENOMINATIONS[d]);
        let n = left.wrapping_div(&unit);
        if n == U1024::ZERO {
            continue;
        }
        counts[d] = n.as_words()[0];
        left = left.wrapping_sub(&n.wrapping_mul(&unit));
        if left == U1024::ZERO {
            break;
        }
    }
    counts
}
/// UTXOs of `count[d]` outputs per denomination above `min`, in descending
/// order, while `gas` (if any) pays 9,000 per output and indices stay
/// below 65,535.
fn emit(counts: &[u64; 15], min: usize, mut gas: Option<u64>, mut each: impl FnMut(u64, u8)) {
    let mut index = 0u64;
    for d in (min..15).rev() {
        for _ in 0..counts[d] {
            if gas.is_some_and(|g| g < 9000) || index >= u16::MAX as u64 {
                break;
            }
            if let Some(g) = gas.as_mut() {
                *g -= 9000;
            }
            each(index, d as u8);
            index += 1;
        }
    }
}
fn number_be(n: u64) -> Vec<u8> {
    magnitude(U1024::from_u64(n))
}
/// `GetDeltas` of one block.
fn deltas(
    view: &dyn ChainView,
    block: &BlockData,
    wanted: &BTreeSet<Address>,
    out: &mut Deltas,
) -> R<()> {
    use quai_primitives::transaction::Kind;
    for s in view.spent_utxos(block)? {
        if wanted.contains(&s.address) {
            add(
                out,
                &s.address,
                false,
                &s.tx_hash,
                output(s.index as u64, s.denomination, &s.lock),
            );
        }
    }
    let number = block.number;
    let prime = share_rules::number(
        block
            .header
            .wire()
            .prime_terminus_number
            .as_deref()
            .unwrap_or_default(),
    )
    .unwrap_or_default();
    for (tx, hash) in block.object.transactions().iter().zip(&block.tx_hashes) {
        match tx.kind() {
            Kind::Qi(q) => {
                for (i, o) in q.outputs.iter().enumerate() {
                    let raw = o.address.as_deref().unwrap_or_default();
                    // A conversion output (`BytesToAddress`: the last 20 bytes).
                    if json::address20(raw)[1] <= 127 {
                        continue;
                    }
                    // `common.AddressBytes(out.Address)`: the first 20 bytes;
                    // a shorter address panics.
                    let a: Address = raw
                        .get(..20)
                        .and_then(|b| b.try_into().ok())
                        .ok_or_else(crashed)?;
                    if wanted.contains(&a) {
                        add(
                            out,
                            &a,
                            true,
                            hash,
                            output(i as u64, o.denomination, &o.lock),
                        );
                    }
                }
            }
            Kind::External(e) => {
                let to_qi = e.to[1] > 127;
                match e.etx_type {
                    // UnwrapQiType
                    6 if to_qi && wanted.contains(&e.to) => {
                        let lock = number_be(spec::FORKS.unwrap_qi_lock_period(prime) + number);
                        let counts = min_denominations(wide(&e.value));
                        emit(&counts, 6, Some(e.gas), |i, d| {
                            add(out, &e.to, true, hash, output(i, d, &lock))
                        });
                    }
                    // CoinbaseType
                    1 if to_qi => {
                        let Some(&lock_byte) = e.data.first() else {
                            continue;
                        };
                        if !wanted.contains(&e.to) {
                            continue;
                        }
                        let depth = *spec::LOCK_PERIODS
                            .by_lock_byte
                            .get(lock_byte as usize)
                            .ok_or_else(crashed)?;
                        let lock = number_be(depth + number);
                        let value = quai_execution::economic_state::reward(
                            from_bytes(&e.value),
                            lock_byte,
                            number,
                        )
                        .map_err(|_| crashed())?;
                        let counts = min_denominations(from_uint(value));
                        emit(&counts, 0, None, |i, d| {
                            add(out, &e.to, true, hash, output(i, d, &lock))
                        });
                    }
                    // ConversionType
                    2 if to_qi && wanted.contains(&e.to) => {
                        let lock = number_be(spec::LOCK_PERIODS.conversion + number);
                        if e.gas < 21000 {
                            continue;
                        }
                        let counts = min_denominations(wide(&e.value));
                        emit(&counts, 0, Some(e.gas - 21000), |i, d| {
                            add(out, &e.to, true, hash, output(i, d, &lock))
                        });
                    }
                    // ConversionRevertType
                    5 if !to_qi => {
                        let lock = number_be(spec::LOCK_PERIODS.conversion + number);
                        // `tx.Data()[2:22]` panics on shorter data.
                        let mut a: Address = e
                            .data
                            .get(2..22)
                            .ok_or_else(crashed)?
                            .try_into()
                            .map_err(|_| crashed())?;
                        a[16..].copy_from_slice(&(number as u32).to_be_bytes());
                        if !wanted.contains(&a) || e.gas < 21000 {
                            continue;
                        }
                        let counts = min_denominations(wide(&e.value));
                        emit(&counts, 6, Some(e.gas - 21000), |i, d| {
                            add(out, &a, true, hash, output(i, d, &lock))
                        });
                    }
                    _ => {}
                }
            }
            Kind::Quai(_) => {}
        }
    }
    Ok(())
}
fn parent_hash(block: &BlockData) -> Hash {
    block
        .header
        .wire()
        .parent_hash
        .as_ref()
        .and_then(|h| h.value.as_slice().try_into().ok())
        .unwrap_or_default()
}
fn is_canonical(view: &dyn ChainView, block: &BlockData) -> R<bool> {
    if block.number > view.head() {
        return Ok(false);
    }
    Ok(view
        .block_by_number(block.number)?
        .is_some_and(|b| b.hash == block.hash))
}
fn check_cancel(cancel: Option<&AtomicBool>) -> R<()> {
    if cancel.is_some_and(|c| c.load(AtomicOrdering::Relaxed)) {
        return Err(RpcError {
            code: -32002,
            message: "request timed out".into(),
            data: None,
        });
    }
    Ok(())
}
/// `rawdb.FindCommonAncestor`; a missing header panics in Go.
fn common_ancestor(
    view: &dyn ChainView,
    mut a: Arc<BlockData>,
    mut b: Arc<BlockData>,
    cancel: Option<&AtomicBool>,
) -> R<Arc<BlockData>> {
    let genesis = view.block_by_number(0)?.map(|g| g.hash);
    let is_genesis = |h: &Hash| Some(*h) == genesis;
    let mut steps = 0usize;
    let mut step = || -> R<()> {
        check_cancel(cancel)?;
        steps += 1;
        if steps > MAX_COMMON_ANCESTOR_STEPS {
            return Err(RpcError::go("common ancestor search exceeded limit"));
        }
        Ok(())
    };
    let parent = |x: &BlockData| -> R<Arc<BlockData>> {
        view.block_or_candidate(&parent_hash(x))?
            .ok_or_else(crashed)
    };
    while a.number > b.number {
        step()?;
        a = parent(&a)?;
        if is_genesis(&a.hash) {
            return Ok(a);
        }
    }
    while a.number < b.number {
        step()?;
        b = parent(&b)?;
        if is_genesis(&b.hash) {
            return Ok(b);
        }
    }
    while a.hash != b.hash {
        step()?;
        a = parent(&a)?;
        b = parent(&b)?;
        if is_genesis(&parent_hash(&a)) && is_genesis(&parent_hash(&b)) {
            return parent(&a);
        }
    }
    Ok(a)
}
/// `GetOutpointDeltasForAddressesInRange`: the outputs that blocks
/// `from..=to` created for and removed from the addresses, by address,
/// "created"/"deleted" and transaction hash.
fn outpoint_deltas(view: &dyn ChainView, params: &Value, cancel: Option<&AtomicBool>) -> R<Value> {
    let a = Args::new(params, 3)?;
    let addresses = a.get(0, address_list)?;
    let from = a.get(1, args::hash)?;
    let to = a.get(2, args::hash)?;
    if addresses.is_empty() {
        return Err(RpcError::go("addresses cannot be empty"));
    }
    if addresses.len() > MAX_OUTPOINT_DELTA_ADDRESSES {
        return Err(RpcError::go("too many addresses"));
    }
    let not_found = |h: &Hash| {
        RpcError::go(format!(
            "block {} not found",
            quai_primitives::encode_hex(h)
        ))
    };
    let block_from = view
        .block_or_candidate(&from)?
        .ok_or_else(|| not_found(&from))?;
    let block_to = view
        .block_or_candidate(&to)?
        .ok_or_else(|| not_found(&to))?;
    if block_from.number > block_to.number {
        return Err(RpcError::go(
            "from block number is greater than to block number",
        ));
    }
    if block_to.number - block_from.number > 1000 {
        return Err(RpcError::go("range is too large, max range is 1000"));
    }
    let wanted: BTreeSet<Address> = addresses.iter().copied().collect();
    let mut out: Deltas = BTreeMap::new();
    for addr in &addresses {
        out.insert(json::checksum(addr), Default::default());
    }
    let common = if !is_canonical(view, &block_from)? || !is_canonical(view, &block_to)? {
        common_ancestor(view, block_from.clone(), block_to.clone(), cancel)?
    } else {
        block_from.clone()
    };
    let mut current = block_to;
    while current.hash != common.hash {
        check_cancel(cancel)?;
        deltas(view, &current, &wanted, &mut out)?;
        let p = parent_hash(&current);
        current = view.block_or_candidate(&p)?.ok_or_else(|| not_found(&p))?;
    }
    if common.hash != block_from.hash {
        let mut current = block_from;
        while current.hash != common.hash {
            check_cancel(cancel)?;
            deltas(view, &current, &wanted, &mut out)?;
            let p = parent_hash(&current);
            current = view.block_or_candidate(&p)?.ok_or_else(|| not_found(&p))?;
        }
    }
    check_cancel(cancel)?;
    deltas(view, &common, &wanted, &mut out)?;
    let mut result = Map::new();
    for (address, [created, deleted]) in out {
        let section = |m: BTreeMap<String, Vec<Value>>| {
            Value::Object(m.into_iter().map(|(k, v)| (k, Value::Array(v))).collect())
        };
        result.insert(
            address,
            json!({"created": section(created), "deleted": section(deleted)}),
        );
    }
    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn min_denominations_is_greedy() {
        // 1,000,010,007 qits: one 1,000,000,000, one 10,000, one 5, two 1.
        let c = min_denominations(U1024::from_u64(1_000_000_000 + 2 * 5000 + 7));
        assert_eq!(c[14], 1);
        assert_eq!(c[8], 1);
        assert_eq!(c[7], 0);
        assert_eq!(c[1], 1);
        assert_eq!(c[0], 2);
        assert_eq!(c.iter().sum::<u64>(), 5);
    }
    #[test]
    fn qi_gas_counts_etxs_and_conversions() {
        let a = QiArgs {
            tx_type: 2,
            inputs: vec![0],
            outputs: vec![[
                0, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
            ]],
        };
        assert_eq!(
            qi_tx_gas(&a, f64::NEG_INFINITY, [0, 0]).unwrap(),
            800 + 9000 + 3000
        );
        let mut other = a.outputs[0];
        other[0] = 0x10;
        let b = QiArgs {
            outputs: vec![other],
            ..a
        };
        assert_eq!(qi_tx_gas(&b, 1.0, [0, 0]).unwrap(), 12800 + 42000);
    }
}
