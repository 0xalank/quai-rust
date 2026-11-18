//! Quai interpreter with shared transaction state, nested calls, creation,
//! precompiles and outbound ETX components. Unsupported domains return
//! capability errors; complete economic and block admission remain separate.
//!
//! Gas is the only execution bound, as in go-quai v0.57.0. The interpreter has
//! no step, memory, log, call-input or access-list cap of its own:
//!
//! - Block gas: `core.CalcGasLimit` (core/block_validator.go) returns at most
//!   `params.GasCeil` = 50,000,000 on every network, and `verifyHeader`
//!   (core/headerchain_validation.go) requires the header gas limit to equal
//!   it. A transaction cannot use more gas than the block gas pool.
//! - Memory: Go charges `3w + w²/512` gas for `w` words (core/vm/gas_table.go
//!   `memoryGasCost`). Its only other check, `size > 0x1FFFFFFFE0`, is out of
//!   gas here too. One frame with 50,000,000 gas reaches at most 159,233 words
//!   (5,095,456 bytes). Live frames share the gas of one transaction and the
//!   cost is convex, so all 1,025 possible frames together hold at most
//!   1,025 × 4,288 words (140,646,400 bytes).
//! - Steps: Go counts steps only to poll `evm.abort` (core/vm/interpreter.go).
//!   Between two taken jumps the program counter only moves forward, so a
//!   frame runs at most `code.len() + 1` steps per segment, and each taken
//!   JUMP/JUMPI costs at least 8 gas. Every frame therefore halts after at most
//!   `(gas / 8 + 1) * (code.len() + 1)` steps.
//! - Logs: Go has no count or size cap (core/vm/instructions.go `makeLog`).
//!   Gas allows at most 133,333 logs and 6,250,000 data bytes per transaction.
//!
//! `MAX_TRACE_BYTES` applies only when `Context::trace` is set. Consensus
//! callers always clear it, so it cannot change a block result.
pub mod calls;
pub mod etx;
pub mod journal;
pub mod lockup;
pub mod precompile;
pub mod state;
pub mod state_gas;
pub mod word;
use journal::Journal;
use quai_primitives::{
    receipt::Log,
    transaction::{keccak, Address},
    Hash,
};
use state::World;
use std::collections::BTreeSet;
use word::{boolean, bytes, from_bytes, uint, Word};
pub const NEW_OPS_HEIGHT: u64 = 3_490_000;
/// Diagnostic trace budget. Only `Context::trace` frames use it; consensus
/// execution never sets that flag.
pub const MAX_TRACE_BYTES: usize = 64 << 20;
#[derive(Clone, Debug)]
pub struct Context {
    pub address: Address,
    pub caller: Address,
    pub origin: Address,
    pub coinbase: Address,
    pub etx_sender: Address,
    pub location: [u8; 2],
    pub tx_type: u8,
    pub value: Word,
    pub gas_price: Word,
    pub block_number: Word,
    pub timestamp: Word,
    pub difficulty: Word,
    pub base_fee: Word,
    pub chain_id: Word,
    pub state_size: Word,
    pub block_gas_limit: u64,
    pub address_listed: bool,
    pub warm_slots: BTreeSet<Hash>,
    pub read_only: bool,
    pub trace: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapabilityError {
    StateUnavailable(String),
    Opcode(u8),
    Limit(&'static str),
    Context(&'static str),
}
impl std::fmt::Display for CapabilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StateUnavailable(message) => write!(f, "native state unavailable: {message}"),
            _ => write!(f, "unsupported native execution domain: {self:?}"),
        }
    }
}
impl std::error::Error for CapabilityError {}
impl CapabilityError {
    pub(crate) fn state(error: state::ReadError, fallback: Self) -> Self {
        match error {
            state::ReadError::Unavailable(v) => Self::StateUnavailable(v),
            _ => fallback,
        }
    }
}

impl From<state::ReadError> for CapabilityError {
    fn from(error: state::ReadError) -> Self {
        match error {
            state::ReadError::Unavailable(v) => Self::StateUnavailable(v),
            state::ReadError::Invalid(_) => Self::Limit("execution state working domain"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    OutOfGas,
    StackUnderflow,
    StackOverflow,
    InvalidJump,
    WriteProtection,
    ReturnDataBounds,
    GasOverflow,
    InvalidOpcode,
    InvalidAccessList,
    Depth,
    InsufficientBalance,
    ContractCollision,
    InvalidCode,
    CodeStoreOutOfGas,
    AddressGrind,
    ExternalAddress,
    MaxCodeSize,
    Precompile,
    Lockup,
    Etx,
}
impl Fault {
    pub fn class(self) -> &'static str {
        match self {
            Self::OutOfGas => "out_of_gas",
            Self::StackUnderflow => "stack_underflow",
            Self::StackOverflow => "stack_overflow",
            Self::InvalidJump => "invalid_jump",
            Self::WriteProtection => "write_protection",
            Self::ReturnDataBounds => "return_data_bounds",
            Self::GasOverflow => "gas_overflow",
            Self::InvalidOpcode => "invalid_opcode",
            Self::InvalidAccessList => "invalid_access_list",
            Self::Depth => "depth",
            Self::InsufficientBalance => "insufficient_balance",
            Self::ContractCollision => "contract_collision",
            Self::InvalidCode => "invalid_code",
            Self::CodeStoreOutOfGas => "code_store_out_of_gas",
            Self::AddressGrind => "address_grind",
            Self::ExternalAddress => "external_address",
            Self::MaxCodeSize => "max_code_size",
            Self::Lockup => "vm_error",
            Self::Precompile => "precompile",
            Self::Etx => "etx",
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct AuxiliaryEffects {
    pub deleted_hashes: Vec<Hash>,
    pub deleted: std::collections::BTreeMap<crate::lockup::Key, crate::lockup::Lockup>,
}
#[derive(Clone, Debug)]
pub struct Outcome {
    pub auxiliary: AuxiliaryEffects,
    pub error: Option<Fault>,
    pub reverted: bool,
    pub output: Vec<u8>,
    pub gas_remaining: u64,
    pub state_gas: u64,
    pub refund: u64,
    pub logs: Vec<Log>,
    pub outbound_etxs: Vec<quai_primitives::transaction::Transaction>,
    pub trace_hash: Hash,
    pub trace_steps: usize,
}
#[derive(Clone, Copy)]
struct Spec {
    required: usize,
    pop: usize,
    push: usize,
    gas: u64,
    writes: bool,
}
fn spec(op: u8) -> Option<Spec> {
    let (required, pop, push, gas) = match op {
        0x00 => (0, 0, 0, 0),
        0x01 | 0x03 | 0x10..=0x14 | 0x16..=0x18 | 0x1a..=0x1d => (2, 2, 1, 3),
        0x02 | 0x04..=0x07 | 0x0b => (2, 2, 1, 5),
        0x08 | 0x09 => (3, 3, 1, 8),
        0x0a => (2, 2, 1, 0),
        0x15 | 0x19 => (1, 1, 1, 3),
        0x20 => (2, 2, 1, 30),
        0x30 | 0x32..=0x34 | 0x36 | 0x38 | 0x3a | 0x3d | 0x41..=0x46 | 0x48 | 0x58..=0x5a => {
            (0, 0, 1, 2)
        }
        0x35 => (1, 1, 1, 3),
        0x31 | 0x3b | 0x3f => (1, 1, 1, 0),
        0x3c => (4, 4, 0, 0),
        0x40 => (1, 1, 1, 20),
        0xff => (1, 1, 0, 5000),
        0x37 | 0x39 | 0x3e | 0x5e => (3, 3, 0, 3),
        0x47 => (0, 0, 1, 5),
        0x50 => (1, 1, 0, 2),
        0x51 => (1, 1, 1, 3),
        0x52 | 0x53 => (2, 2, 0, 3),
        0x54 => (1, 1, 1, 0),
        0x55 => (2, 2, 0, 0),
        0x56 => (1, 1, 0, 8),
        0x57 => (2, 2, 0, 10),
        0x5b => (0, 0, 0, 1),
        0x5c => (1, 1, 1, 0),
        0x5d => (2, 2, 0, 0),
        0x5f => (0, 0, 1, 2),
        0x60..=0x7f => (0, 0, 1, 3),
        0x80..=0x8f => ((op - 0x7f) as usize, 0, 1, 3),
        0x90..=0x9f => ((op - 0x8f + 1) as usize, 0, 0, 3),
        0xa0..=0xa4 => ((op - 0xa0 + 2) as usize, (op - 0xa0 + 2) as usize, 0, 0),
        0xf0 => (3, 3, 1, 32000),
        0xf5 => (4, 4, 1, 32000),
        0xf1 | 0xf2 => (7, 7, 1, 0),
        0xf4 | 0xfa => (6, 6, 1, 0),
        0xf3 | 0xfd => (2, 2, 0, 0),
        0xf6 => (10, 10, 1, 21000),
        0xf8 => (4, 4, 1, 21000),
        0xf7 => (1, 1, 1, 3),
        _ => return None,
    };
    Some(Spec {
        required,
        pop,
        push,
        gas,
        writes: matches!(
            op,
            0x55 | 0x5d | 0xa0..=0xa4 | 0xf0 | 0xf5 | 0xf6 | 0xf8 | 0xff
        ),
    })
}
pub fn supported_opcodes() -> Vec<u8> {
    (0..=255)
        .filter(|op| {
            spec(*op).is_some()
                && !matches!(
                    *op,
                    0x31 | 0x3b
                        | 0x3c
                        | 0x3f
                        | 0x40
                        | 0xf0
                        | 0xf1
                        | 0xf2
                        | 0xf4
                        | 0xf5
                        | 0xf6
                        | 0xf8
                        | 0xfa
                        | 0xff
                )
        })
        .collect()
}
fn unsupported(op: u8) -> bool {
    matches!(op,0x31|0x3b|0x3c|0x3f|0x40|0xf0..=0xf2|0xf4..=0xf6|0xf8|0xfa|0xff)
}
#[derive(Debug)]
enum Stop {
    Success(Vec<u8>),
    Revert(Vec<u8>),
    Fault(Fault),
    Capability(CapabilityError),
}
impl From<Fault> for Stop {
    fn from(v: Fault) -> Self {
        Self::Fault(v)
    }
}
impl From<CapabilityError> for Stop {
    fn from(v: CapabilityError) -> Self {
        Self::Capability(v)
    }
}
impl From<state::ReadError> for Stop {
    fn from(error: state::ReadError) -> Self {
        Self::Capability(error.into())
    }
}

struct Machine<'a> {
    ctx: &'a Context,
    state: &'a mut Journal,
    depth: usize,
    gas_levels: u128,
    code: Vec<u8>,
    input: &'a [u8],
    stack: Vec<Word>,
    memory: Vec<u8>,
    memory_paid: u64,
    return_data: Vec<u8>,
    call_gas: u64,
    pc: usize,
    gas: u64,
    state_gas: u64,

    jumps: BTreeSet<usize>,
}
impl Machine<'_> {
    fn back(&self, n: usize) -> Word {
        self.stack[self.stack.len() - 1 - n]
    }
    fn pop(&mut self) -> Word {
        self.stack.pop().unwrap()
    }
    fn push(&mut self, v: Word) {
        self.stack.push(v)
    }
    fn cost(&self, rate: u64) -> u64 {
        state_gas::scale_levels(self.gas_levels, rate)
    }
    fn charge(&mut self, amount: u64) -> Result<(), Stop> {
        self.gas = self.gas.checked_sub(amount).ok_or(Fault::OutOfGas)?;
        Ok(())
    }
    fn memory_end(offset: Word, length: Word) -> Result<u64, Stop> {
        if length == Word::ZERO {
            return Ok(0);
        }
        let off = uint(offset).ok_or(Fault::GasOverflow)?;
        let len = uint(length).ok_or(Fault::GasOverflow)?;
        Ok(off.checked_add(len).ok_or(Fault::GasOverflow)?)
    }
    fn needed_memory(&self, op: u8) -> Result<u64, Stop> {
        match op {
            0x51 | 0x52 => Self::memory_end(self.back(0), Word::from_u64(32)),
            0x53 => Self::memory_end(self.back(0), Word::ONE),
            0x20 | 0xf3 | 0xfd | 0xa0..=0xa4 => Self::memory_end(self.back(0), self.back(1)),
            0x37 | 0x39 | 0x3e => Self::memory_end(self.back(0), self.back(2)),
            0xf6 => Ok(Self::memory_end(self.back(6), self.back(7))?
                .checked_add(Self::memory_end(self.back(8), self.back(9))?)
                .ok_or(Fault::GasOverflow)?),
            0x3c => Self::memory_end(self.back(1), self.back(3)),
            0xf0 | 0xf5 => Self::memory_end(self.back(1), self.back(2)),
            0xf1 | 0xf2 => Ok(Self::memory_end(self.back(3), self.back(4))?
                .max(Self::memory_end(self.back(5), self.back(6))?)),
            0xf4 | 0xfa => Ok(Self::memory_end(self.back(2), self.back(3))?
                .max(Self::memory_end(self.back(4), self.back(5))?)),
            0x5e => Ok(Self::memory_end(self.back(0), self.back(2))?
                .max(Self::memory_end(self.back(1), self.back(2))?)),
            _ => Ok(0),
        }
    }
    fn memory_cost(&self, end: u64) -> Result<(u64, u64), Stop> {
        let rounded = end.checked_add(31).ok_or(Fault::GasOverflow)? / 32;
        let size = rounded.checked_mul(32).ok_or(Fault::GasOverflow)?;
        if size > 0x1fffffffe0 {
            return Err(Fault::OutOfGas.into());
        }
        if size <= self.memory.len() as u64 {
            return Ok((0, size));
        }

        Ok((
            3 * rounded + rounded * rounded / 512 - self.memory_paid,
            size,
        ))
    }
    fn storage_cost(&mut self, op: u8) -> Result<(u64, u64), Stop> {
        let slot = bytes(self.back(0));
        let cold = if self.state.slots.contains(&(self.ctx.address, slot)) {
            0
        } else {
            self.cost(2100)
        };
        let warm = self.cost(100);
        if op == 0x54 {
            return Ok((
                if self.state.slots.contains(&(self.ctx.address, slot)) {
                    warm
                } else {
                    cold
                },
                0,
            ));
        }
        if self.gas <= self.cost(2300) {
            return Err(Fault::OutOfGas.into());
        }
        let current = self
            .state
            .world()
            .account(&self.ctx.address)?
            .unwrap()
            .get(slot)?;
        let original = self.state.original(self.ctx.address, slot)?;
        let value = bytes(self.back(1));
        let set = self.cost(20_000);
        let reset = self.cost(5000);
        let cold_rate = self.cost(2100);
        let clear_refund = reset - cold_rate + 1900;
        if current == value {
            return Ok((cold + warm, cold));
        }
        if original == current {
            if original == [0; 32] {
                return Ok((cold + set, cold));
            }
            if value == [0; 32] {
                self.state.refund += clear_refund;
            }
            return Ok((cold + reset - cold_rate, cold));
        }
        if original != [0; 32] {
            if current == [0; 32] {
                self.state.refund = self
                    .state
                    .refund
                    .checked_sub(clear_refund)
                    .ok_or(CapabilityError::Context("refund underflow"))?;
            } else if value == [0; 32] {
                self.state.refund += clear_refund;
            }
        }
        if original == value {
            self.state.refund += if original == [0; 32] {
                set - warm
            } else {
                reset - cold_rate - warm
            };
        }
        Ok((cold + warm, cold))
    }
    fn trace(&mut self, op: u8, gas_before: u64) -> Result<(), Stop> {
        if !self.ctx.trace {
            return Ok(());
        }
        self.state.trace_bytes = self
            .state
            .trace_bytes
            .checked_add(self.memory.len() + self.stack.len() * 32 + 128)
            .ok_or(CapabilityError::Limit("trace bytes"))?;
        if self.state.trace_bytes > MAX_TRACE_BYTES {
            return Err(CapabilityError::Limit("trace bytes").into());
        }
        let mut data = Vec::with_capacity(self.stack.len() * 32 + 128);
        data.extend(self.state.trace_hash);
        data.extend((self.pc as u64).to_be_bytes());
        data.push(op);
        data.extend(gas_before.to_be_bytes());
        data.extend(self.state_gas.to_be_bytes());
        data.extend(self.state.refund.to_be_bytes());
        data.extend((self.stack.len() as u64).to_be_bytes());
        for v in &self.stack {
            data.extend(bytes(*v));
        }
        data.extend((self.memory.len() as u64).to_be_bytes());
        data.extend(keccak(&self.memory));
        self.state.trace_hash = keccak(&data);
        self.state.trace_steps += 1;
        Ok(())
    }
    fn read_data(source: &[u8], offset: Word, length: usize) -> Vec<u8> {
        let mut result = vec![0; length];
        if let Some(off) = uint(offset).and_then(|n| usize::try_from(n).ok()) {
            if off < source.len() {
                let n = length.min(source.len() - off);
                result[..n].copy_from_slice(&source[off..off + n]);
            }
        }
        result
    }
    fn range(&self, offset: Word, length: Word) -> (usize, usize) {
        let len = uint(length).unwrap() as usize;
        if len == 0 {
            (0, 0)
        } else {
            (uint(offset).unwrap() as usize, len)
        }
    }
    fn step(&mut self) -> Result<(), Stop> {
        let op = self.code.get(self.pc).copied().unwrap_or(0);
        let gas_before = self.gas;
        if matches!(
            op,
            0x31 | 0x3b
                | 0x3c
                | 0x3f
                | 0x40
                | 0xf0
                | 0xf1
                | 0xf2
                | 0xf4
                | 0xf5
                | 0xf6
                | 0xf8
                | 0xfa
                | 0xff
        ) && !self.state.allow_calls
        {
            return Err(CapabilityError::Opcode(op).into());
        }
        let Some(instruction) = spec(op) else {
            if unsupported(op) {
                return Err(CapabilityError::Opcode(op).into());
            }
            return Err(Fault::InvalidOpcode.into());
        };
        if matches!(op, 0x5c..=0x5f) && self.ctx.block_number < Word::from_u64(NEW_OPS_HEIGHT) {
            return Err(Fault::InvalidOpcode.into());
        }
        if self.stack.len() < instruction.required {
            return Err(Fault::StackUnderflow.into());
        }
        if self.stack.len() - instruction.pop + instruction.push > 1024 {
            return Err(Fault::StackOverflow.into());
        }
        if self.ctx.read_only && (instruction.writes || (op == 0xf1 && self.back(2) != Word::ZERO))
        {
            return Err(Fault::WriteProtection.into());
        }
        let static_gas = if matches!(
            op,
            0x5c | 0x5d | 0x31 | 0x3b | 0x3c | 0x3f | 0xf1 | 0xf2 | 0xf4 | 0xfa
        ) {
            self.cost(100)
        } else {
            instruction.gas
        };
        self.charge(static_gas)?;
        if matches!(
            op,
            0x5c | 0x5d | 0x31 | 0x3b | 0x3c | 0x3f | 0xf1 | 0xf2 | 0xf4 | 0xfa
        ) {
            self.state_gas += static_gas;
        }
        let end = self.needed_memory(op)?;
        let (memory_gas, size) = self.memory_cost(end)?;
        let mut dynamic = if op == 0xf6 { 0 } else { memory_gas };
        let mut state_cost = 0;
        match op {
            0x31 | 0x3b | 0x3c | 0x3f => {
                let b = bytes(self.back(0));
                let a: Address = b[12..].try_into().unwrap();
                let cold = if self.state.addresses.contains(&a) {
                    0
                } else {
                    self.cost(2600) - self.cost(100)
                };
                dynamic = dynamic.checked_add(cold).ok_or(Fault::GasOverflow)?;
                state_cost = cold;
                if op == 0x3c {
                    dynamic = dynamic
                        .checked_add(
                            uint(self.back(3))
                                .ok_or(Fault::OutOfGas)?
                                .div_ceil(32)
                                .checked_mul(3)
                                .ok_or(Fault::GasOverflow)?,
                        )
                        .ok_or(Fault::GasOverflow)?;
                    if cold != 0 {
                        state_cost = cold + 2 * self.cost(100);
                    }
                }
            }
            0xff => {
                let b = bytes(self.back(0));
                let a: Address = b[12..].try_into().unwrap();
                if !calls::local(&a, self.ctx.location) {
                    return Err(Fault::ExternalAddress.into());
                }
                if !self.state.addresses.contains(&a) {
                    dynamic = self.cost(2600);
                }
                if calls::empty(self.state, &a)?
                    && self
                        .state
                        .world()
                        .account(&self.ctx.address)?
                        .unwrap()
                        .balance
                        != Word::ZERO
                {
                    dynamic = dynamic.checked_add(25000).ok_or(Fault::GasOverflow)?;
                }
            }
            0xf5 => {
                dynamic = dynamic
                    .checked_add(
                        uint(self.back(2))
                            .ok_or(Fault::OutOfGas)?
                            .div_ceil(32)
                            .checked_mul(6)
                            .ok_or(Fault::GasOverflow)?,
                    )
                    .ok_or(Fault::GasOverflow)?
            }
            0x0a => dynamic = 10 + ((self.back(1).bits_vartime() as u64 + 7) / 8) * 50,
            0x20 => dynamic += uint(self.back(1)).ok_or(Fault::OutOfGas)?.div_ceil(32) * 6,
            0x37 | 0x39 | 0x3e | 0x5e => {
                dynamic = dynamic
                    .checked_add(
                        uint(self.back(2))
                            .ok_or(Fault::OutOfGas)?
                            .div_ceil(32)
                            .checked_mul(3)
                            .ok_or(Fault::OutOfGas)?,
                    )
                    .ok_or(Fault::OutOfGas)?
            }
            0xf1 | 0xf2 | 0xf4 | 0xfa => {
                let raw = bytes(self.back(1));
                let address: Address = raw[12..].try_into().unwrap();
                if op == 0xf1 && !calls::local(&address, self.ctx.location) {
                    return Err(Fault::OutOfGas.into());
                }
                let cold = if self.state.addresses.contains(&address) {
                    0
                } else {
                    self.cost(2600) - self.cost(100)
                };
                dynamic = dynamic.checked_add(cold).ok_or(Fault::GasOverflow)?;
                state_cost = cold * 2;
                if matches!(op, 0xf1 | 0xf2) && self.back(2) != Word::ZERO {
                    dynamic = dynamic.checked_add(9000).ok_or(Fault::GasOverflow)?;
                    if op == 0xf1 && calls::empty(self.state, &address)? {
                        dynamic = dynamic
                            .checked_add(state_gas::scaled(self.ctx.state_size, Word::ZERO, 25000))
                            .ok_or(Fault::GasOverflow)?;
                    }
                }
                let available = self.gas.checked_sub(dynamic).ok_or(Fault::OutOfGas)?;
                let cap = available - available / 64;
                self.call_gas = uint(self.back(0)).unwrap_or(u64::MAX).min(cap);
                dynamic = dynamic
                    .checked_add(self.call_gas)
                    .ok_or(Fault::GasOverflow)?;
            }
            0x54 | 0x55 => {
                let (g, s) = self.storage_cost(op)?;
                dynamic = g;
                state_cost = s;
            }
            0xa0..=0xa4 => {
                dynamic = dynamic
                    .checked_add(375 + (op as u64 - 0xa0) * 375)
                    .and_then(|n| {
                        uint(self.back(1))
                            .and_then(|v| v.checked_mul(8))
                            .and_then(|v| n.checked_add(v))
                    })
                    .ok_or(Fault::OutOfGas)?
            }
            _ => {}
        }
        self.charge(dynamic)?;
        if op != 0xf6 && size > self.memory.len() as u64 {
            let words = size / 32;
            self.memory_paid = 3 * words + words * words / 512;
        }
        self.state_gas += state_cost;
        if size > self.memory.len() as u64 {
            self.memory.resize(size as usize, 0);
        }
        self.trace(op, gas_before)?;
        match op {
            0x00 => return Err(Stop::Success(vec![])),
            0x01..=0x0b | 0x10..=0x1d => {
                let a = self.pop();
                let value = match op {
                    0x15 => boolean(a == Word::ZERO),
                    0x19 => a.wrapping_xor(&Word::MAX),
                    _ => {
                        let b = self.pop();
                        match op {
                            0x01 => a.wrapping_add(&b),
                            0x02 => a.wrapping_mul(&b),
                            0x03 => a.wrapping_sub(&b),
                            0x04 => word::div(a, b),
                            0x05 => word::signed_div(a, b),
                            0x06 => word::rem(a, b),
                            0x07 => word::signed_rem(a, b),
                            0x08 => {
                                let m = self.pop();
                                word::addmod(a, b, m)
                            }
                            0x09 => {
                                let m = self.pop();
                                word::mulmod(a, b, m)
                            }
                            0x0a => word::pow(a, b),
                            0x0b => word::sign_extend(b, a),
                            0x10 => boolean(a < b),
                            0x11 => boolean(a > b),
                            0x12 => boolean(word::signed_lt(a, b)),
                            0x13 => boolean(word::signed_lt(b, a)),
                            0x14 => boolean(a == b),
                            0x16 => a.wrapping_and(&b),
                            0x17 => a.wrapping_or(&b),
                            0x18 => a.wrapping_xor(&b),
                            0x1a => Word::from_u64(
                                uint(a)
                                    .filter(|n| *n < 32)
                                    .map(|n| bytes(b)[n as usize] as u64)
                                    .unwrap_or(0),
                            ),
                            0x1b => uint(a)
                                .filter(|n| *n < 256)
                                .map(|n| b.shl_vartime(n as usize))
                                .unwrap_or(Word::ZERO),
                            0x1c => uint(a)
                                .filter(|n| *n < 256)
                                .map(|n| b.shr_vartime(n as usize))
                                .unwrap_or(Word::ZERO),
                            0x1d => word::sar(b, a),
                            _ => unreachable!(),
                        }
                    }
                };
                self.push(value);
            }
            0x20 => {
                let off = self.pop();
                let len = self.pop();
                let (o, n) = self.range(off, len);
                self.push(from_bytes(&keccak(&self.memory[o..o + n])));
            }
            0x30 => self.push(from_bytes(&self.ctx.address)),
            0x31 | 0x3b | 0x3c | 0x3f => {
                let b = bytes(self.pop());
                let a: Address = b[12..].try_into().unwrap();
                if op != 0x3b && !calls::local(&a, self.ctx.location) {
                    return Err(Fault::ExternalAddress.into());
                }
                if !self.state.addresses.contains(&a) {
                    return Err(Fault::InvalidAccessList.into());
                }
                let account = if calls::local(&a, self.ctx.location) {
                    self.state.world().account(&a)?
                } else {
                    None
                };
                match op {
                    0x31 => self.push(account.map(|v| v.balance).unwrap_or(Word::ZERO)),
                    0x3b => self.push(Word::from_u64(
                        account.map(|v| v.code.len()).unwrap_or(0) as u64
                    )),
                    0x3f => self.push(if calls::empty(self.state, &a)? {
                        Word::ZERO
                    } else {
                        from_bytes(&keccak(&account.unwrap().code))
                    }),
                    _ => {
                        let code = account.map(|v| v.code.clone()).unwrap_or_default();
                        let off = self.pop();
                        let src = self.pop();
                        let len = self.pop();
                        let (o, n) = self.range(off, len);
                        self.memory[o..o + n].copy_from_slice(&Self::read_data(&code, src, n));
                    }
                }
            }
            0x40 => {
                let requested = uint(self.pop());
                let current = uint(self.ctx.block_number).unwrap_or(u64::MAX);
                let h = if let Some(n) = requested.filter(|n| *n < current && current - *n <= 256) {
                    *self
                        .state
                        .block_hashes
                        .get(&n)
                        .ok_or(CapabilityError::Context("BLOCKHASH ancestry unavailable"))?
                } else {
                    [0; 32]
                };
                self.push(from_bytes(&h));
            }
            0x32 => self.push(from_bytes(&self.ctx.origin)),
            0x33 => self.push(from_bytes(if self.ctx.tx_type == 1 {
                &self.ctx.etx_sender
            } else {
                &self.ctx.caller
            })),
            0x34 => self.push(self.ctx.value),
            0x35 => {
                let off = self.pop();
                self.push(from_bytes(&Self::read_data(self.input, off, 32)));
            }
            0x36 => self.push(Word::from_u64(self.input.len() as u64)),
            0x38 => self.push(Word::from_u64(self.code.len() as u64)),
            0x3a => self.push(self.ctx.gas_price),
            0x3d => self.push(Word::from_u64(self.return_data.len() as u64)),
            0x37 | 0x39 => {
                let off = self.pop();
                let src = self.pop();
                let len = self.pop();
                let (o, n) = self.range(off, len);
                let value =
                    Self::read_data(if op == 0x37 { self.input } else { &self.code }, src, n);
                self.memory[o..o + n].copy_from_slice(&value);
            }
            0x3e => {
                let off = self.pop();
                let offset = self.pop();
                let len = self.pop();
                let start = uint(offset).ok_or(Fault::ReturnDataBounds)?;
                let count = uint(len).ok_or(Fault::ReturnDataBounds)?;
                if start
                    .checked_add(count)
                    .is_none_or(|n| n > self.return_data.len() as u64)
                {
                    return Err(Fault::ReturnDataBounds.into());
                }
                let (o, n) = self.range(off, len);
                self.memory[o..o + n]
                    .copy_from_slice(&self.return_data[start as usize..start as usize + n]);
            }
            0x41 => self.push(from_bytes(&self.ctx.coinbase)),
            0x42 => self.push(self.ctx.timestamp),
            0x43 => self.push(self.ctx.block_number),
            0x44 => self.push(self.ctx.difficulty),
            0x45 => self.push(Word::from_u64(self.ctx.block_gas_limit)),
            0x46 => self.push(self.ctx.chain_id),
            0x47 => self.push(
                self.state
                    .world()
                    .account(&self.ctx.address)?
                    .unwrap()
                    .balance,
            ),
            0x48 => self.push(self.ctx.base_fee),
            0x50 => {
                self.pop();
            }
            0x51 => {
                let off = self.pop();
                let o = uint(off).unwrap() as usize;
                self.push(from_bytes(&self.memory[o..o + 32]));
            }
            0x52 => {
                let off = self.pop();
                let v = self.pop();
                let o = uint(off).unwrap() as usize;
                self.memory[o..o + 32].copy_from_slice(&bytes(v));
            }
            0x53 => {
                let off = self.pop();
                let v = self.pop();
                self.memory[uint(off).unwrap() as usize] = bytes(v)[31];
            }
            0x54 | 0x55 => {
                if !self.state.addresses.contains(&self.ctx.address) {
                    return Err(Fault::InvalidAccessList.into());
                }
                let key = bytes(self.pop());
                if op == 0x54 {
                    self.push(from_bytes(
                        &self
                            .state
                            .world()
                            .account(&self.ctx.address)?
                            .unwrap()
                            .get(key)?,
                    ));
                } else {
                    let v = bytes(self.pop());
                    self.state.set_storage(self.ctx.address, key, v)?;
                }
            }
            0x56 | 0x57 => {
                let dest = self.pop();
                let jump = if op == 0x57 {
                    self.pop() != Word::ZERO
                } else {
                    true
                };
                if jump {
                    let pc = uint(dest)
                        .and_then(|n| usize::try_from(n).ok())
                        .filter(|n| self.jumps.contains(n))
                        .ok_or(Fault::InvalidJump)?;
                    self.pc = pc;
                    return Ok(());
                }
            }
            0x58 => self.push(Word::from_u64(self.pc as u64)),
            0x59 => self.push(Word::from_u64(self.memory.len() as u64)),
            0x5a => self.push(Word::from_u64(self.gas)),
            0x5b => {}
            0x5c => {
                let key = bytes(self.pop());
                self.push(from_bytes(&self.state.transient(self.ctx.address, key)));
            }
            0x5d => {
                let key = bytes(self.pop());
                let value = bytes(self.pop());
                self.state.set_transient(self.ctx.address, key, value)?;
            }
            0x5e => {
                let dest = self.pop();
                let src = self.pop();
                let len = self.pop();
                let (d, n) = self.range(dest, len);
                if n != 0 {
                    let s = uint(src).unwrap() as usize;
                    self.memory.copy_within(s..s + n, d);
                }
            }
            0x5f => self.push(Word::ZERO),
            0x60..=0x7f => {
                let n = (op - 0x5f) as usize;
                let mut value = vec![0; n];
                let start = (self.pc + 1).min(self.code.len());
                let count = n.min(self.code.len() - start);
                value[..count].copy_from_slice(&self.code[start..start + count]);
                self.push(from_bytes(&value));
                self.pc += n;
            }
            0x80..=0x8f => self.push(self.back((op - 0x80) as usize)),
            0x90..=0x9f => {
                let a = self.stack.len() - 1;
                let b = a - (op - 0x8f) as usize;
                self.stack.swap(a, b);
            }
            0xa0..=0xa4 => {
                let off = self.pop();
                let len = self.pop();
                let (o, n) = self.range(off, len);
                let mut topics = Vec::new();
                for _ in 0..op - 0xa0 {
                    topics.push(bytes(self.pop()));
                }
                self.state.logs.push(Log {
                    address: self.ctx.address,
                    topics,
                    data: self.memory[o..o + n].to_vec(),
                });
            }
            0xff => {
                let b = bytes(self.pop());
                let a: Address = b[12..].try_into().unwrap();
                if !self.state.addresses.contains(&a)
                    || !self.state.addresses.contains(&self.ctx.address)
                {
                    return Err(Fault::InvalidAccessList.into());
                }
                let balance = self
                    .state
                    .world()
                    .account(&self.ctx.address)?
                    .unwrap()
                    .balance;
                let before = self
                    .state
                    .world()
                    .account(&a)?
                    .map(|v| v.balance)
                    .unwrap_or(Word::ZERO);
                let mut credit = balance;
                if !self.state.has_suicided(self.ctx.address)
                    || self
                        .state
                        .prime_number
                        .ok_or(CapabilityError::Context("SELFDESTRUCT prime context"))?
                        < 1_919_500
                {
                    let rent =
                        Word::from_u64(state_gas::scaled(self.ctx.state_size, Word::ZERO, 25000))
                            .wrapping_mul(&self.ctx.base_fee);
                    credit = credit.wrapping_add(&rent);
                }
                let after = before.wrapping_add(&credit);
                if after < before {
                    return Err(CapabilityError::Context("balance overflow").into());
                }
                self.state.set_balance(a, after)?;
                self.state.suicide(self.ctx.address)?;
                return Err(Stop::Success(vec![]));
            }
            0xf0 | 0xf5 => {
                let value = self.pop();
                let off = self.pop();
                let len = self.pop();
                let salt = if op == 0xf5 {
                    Some(bytes(self.pop()))
                } else {
                    None
                };
                let (o, n) = self.range(off, len);
                let code = self.memory[o..o + n].to_vec();
                let gas = self.gas - self.gas / 64;
                self.charge(gas)?;
                let (out, address) = calls::create(
                    self.state,
                    self.ctx,
                    &code,
                    value,
                    gas,
                    salt,
                    self.depth + 1,
                )?;
                self.push(if out.error.is_none() && !out.reverted {
                    from_bytes(&address.unwrap())
                } else {
                    Word::ZERO
                });
                self.gas = self
                    .gas
                    .checked_add(out.gas_remaining)
                    .ok_or(Fault::GasOverflow)?;
                self.state_gas = self
                    .state_gas
                    .checked_add(out.state_gas)
                    .ok_or(Fault::GasOverflow)?;
                self.return_data = if out.reverted { out.output } else { vec![] };
            }
            0xf1 | 0xf2 | 0xf4 | 0xfa => {
                self.pop();
                let raw = bytes(self.pop());
                let target: Address = raw[12..].try_into().unwrap();
                let value = if matches!(op, 0xf1 | 0xf2) {
                    self.pop()
                } else {
                    Word::ZERO
                };
                let off = self.pop();
                let len = self.pop();
                let ret_off = self.pop();
                let ret_len = self.pop();
                if !self.state.addresses.contains(&target) {
                    return Err(Fault::InvalidAccessList.into());
                }
                let (o, n) = self.range(off, len);
                let args = self.memory[o..o + n].to_vec();
                let kind = match op {
                    0xf1 => calls::Kind::Call,
                    0xf2 => calls::Kind::CallCode,
                    0xf4 => calls::Kind::Delegate,
                    _ => calls::Kind::Static,
                };
                let child_gas = self.call_gas + if value == Word::ZERO { 0 } else { 2300 };
                let result = calls::invoke(
                    self.state,
                    self.ctx,
                    kind,
                    target,
                    value,
                    &args,
                    child_gas,
                    self.depth + 1,
                )?;
                self.push(boolean(result.error.is_none() && !result.reverted));
                if result.error.is_none() {
                    let (o, n) = self.range(ret_off, ret_len);
                    let count = n.min(result.output.len());
                    self.memory[o..o + count].copy_from_slice(&result.output[..count]);
                }
                self.return_data = result.output;
                self.gas = self
                    .gas
                    .checked_add(result.gas_remaining)
                    .ok_or(Fault::GasOverflow)?;
                if op == 0xf1 {
                    self.state_gas = self
                        .state_gas
                        .checked_add(result.state_gas)
                        .ok_or(Fault::GasOverflow)?;
                }
            }
            0xf3 | 0xfd => {
                let off = self.pop();
                let len = self.pop();
                let (o, n) = self.range(off, len);
                let value = self.memory[o..o + n].to_vec();
                return Err(if op == 0xf3 {
                    Stop::Success(value)
                } else {
                    Stop::Revert(value)
                });
            }
            0xf6 => {
                self.pop();
                let b = bytes(self.pop());
                let to: Address = b[12..].try_into().unwrap();
                let value = self.pop();
                let gas = self.pop();
                let tip = self.pop();
                let cap = self.pop();
                let off = self.pop();
                let len = self.pop();
                let access_off = self.pop();
                let access_len = self.pop();
                let (o, n) = self.range(off, len);
                let data = self.memory[o..o + n].to_vec();
                let (o, n) = self.range(access_off, access_len);
                let access = self.memory[o..o + n].to_vec();
                if let Some(result) = etx::opcode(
                    self.state, self.ctx, to, value, gas, tip, cap, data, &access,
                )? {
                    self.push(boolean(result));
                }
            }
            0xf8 => {
                self.pop();
                let b = bytes(self.pop());
                let to: Address = b[12..].try_into().unwrap();
                let value = self.pop();
                let gas = self.pop();
                let result = etx::convert(self.state, self.ctx, to, value, gas)?;
                self.push(boolean(result));
            }
            0xf7 => {
                let value = bytes(self.pop());
                let addr = &value[12..];
                self.push(boolean(
                    addr[0] == (self.ctx.location[0] << 4 | self.ctx.location[1]),
                ));
            }
            _ => unreachable!(),
        }
        self.pc += 1;
        Ok(())
    }
}
/// Execute one isolated, top-level frame against an existing local Quai account.
/// This omits value transfers, transaction nonces/fees, calls, creation and
/// precompiles. Each invocation starts fresh original/transient/refund state.
/// State and logs commit only on success; all faults/reverts/capability errors
/// leave the caller's world unchanged. No result establishes block validity.
pub fn execute_frame(
    world: &mut World,
    context: &Context,
    input: &[u8],
    gas: u64,
) -> Result<Outcome, CapabilityError> {
    if context.location.iter().any(|n| *n > 15)
        || context.address[0] != (context.location[0] << 4 | context.location[1])
        || context.address[1] & 0x80 != 0
    {
        return Err(CapabilityError::Context("local Quai contract required"));
    }
    world
        .validate()
        .map_err(|error| CapabilityError::state(error, CapabilityError::Limit("world snapshot")))?;
    let code = world
        .account(&context.address)?
        .ok_or(CapabilityError::Context("existing account required"))?
        .code
        .clone();
    let mut state = Journal::new(world.clone())?;
    if context.address_listed {
        state.addresses.insert(context.address);
    }
    state
        .slots
        .extend(context.warm_slots.iter().map(|k| (context.address, *k)));
    let checkpoint = state.snapshot()?;
    let outcome = run_code(&mut state, context, input, gas, code, 0)?;
    if outcome.error.is_none() && !outcome.reverted {
        state.finalize_sizes()?;
        state.commit(checkpoint)?;
        *world = state.into_world()?;
    } else {
        state.revert(checkpoint)?;
    }
    Ok(outcome)
}
pub(crate) fn run_code(
    state: &mut Journal,
    context: &Context,
    input: &[u8],
    gas: u64,
    code: Vec<u8>,
    depth: usize,
) -> Result<Outcome, CapabilityError> {
    let initial_size = state
        .world()
        .account(&context.address)?
        .map(|a| a.storage_size)
        .unwrap_or(Word::ZERO);
    let mut jumps = BTreeSet::new();
    let mut pc = 0;
    while pc < code.len() {
        let op = code[pc];
        if op == 0x5b {
            jumps.insert(pc);
        }
        pc += 1;
        if (0x60..=0x7f).contains(&op) {
            pc += (op - 0x5f) as usize;
        }
    }
    let mut machine = Machine {
        ctx: context,
        state,
        depth,
        gas_levels: state_gas::levels(context.state_size, initial_size),
        code,
        input,
        stack: vec![],
        memory: vec![],
        memory_paid: 0,
        return_data: vec![],
        call_gas: 0,
        pc: 0,
        gas,
        state_gas: 0,

        jumps,
    };
    // Gas bounds the loop: see the module documentation for the halting argument.
    let stop = if machine.code.is_empty() {
        Stop::Success(vec![])
    } else {
        loop {
            if let Err(stop) = machine.step() {
                break stop;
            }
        }
    };
    let (error, reverted, output, success) = match stop {
        Stop::Success(data) => (None, false, data, true),
        Stop::Revert(data) => (None, true, data, false),
        Stop::Fault(error) => (Some(error), false, vec![], false),
        Stop::Capability(error) => return Err(error),
    };
    let outcome = Outcome {
        auxiliary: Default::default(),
        error,
        reverted,
        output,
        gas_remaining: if error.is_some() { 0 } else { machine.gas },
        state_gas: machine.state_gas,
        refund: if success { machine.state.refund } else { 0 },
        logs: if success {
            machine.state.logs.clone()
        } else {
            vec![]
        },
        outbound_etxs: if success {
            machine.state.etxs.clone()
        } else {
            vec![]
        },
        trace_hash: machine.state.trace_hash,
        trace_steps: machine.state.trace_steps,
    };
    Ok(outcome)
}
