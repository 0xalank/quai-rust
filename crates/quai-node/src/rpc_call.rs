//! Read-only message calls for `quai_call`, `quai_estimateGas` and
//! `quai_createAccessList`. go-quai runs them through `GetEVM` and
//! `core.ApplyMessage` with gas price zero and `vm.Config{Debug: true,
//! NoBaseFee: true}` (internal/quaiapi/api.go `DoCall`, `AccessList`).
//! `call` builds the block context of `NewEVMBlockContext` (core/evm.go) for
//! a block of the export view; `run` executes the message with
//! `quai_execution::evm::message::apply`, which turns access-list checks off
//! as Go's debug flag does, and discards every change.
use crate::rpc_view::ExportView;
use quai_execution::evm::{
    self,
    access_list::AccessListTracer,
    calls::{self, Environment},
    message::{self, Applied, Message},
    state::World,
    word::{from_bytes, Word},
};
use quai_primitives::{transaction::Address, Hash};
use quai_rpc::view::{
    BlockData, CallError, CallRequest, CallResult, ChainView, TracedAccessList, ViewError,
};
use std::collections::BTreeSet;

fn view_error(e: impl std::fmt::Display) -> CallError {
    CallError::View(ViewError::Unavailable(format!("execution: {e}")))
}

/// Run `request` on `world` at `block`, as `DoCall` (or one round of
/// `AccessList`) does.
pub fn call(
    view: &ExportView,
    world: World,
    block: &BlockData,
    request: &CallRequest,
) -> Result<CallResult, CallError> {
    let context = context(view, block)?;
    let environment = environment(view, block)?;
    run(&world, &context, &environment, request, view.head())
}

/// Run `request` on `world` in the block whose fields `block` holds.
/// `head` is the number of the head block: go-quai's `AccessList` grinds a
/// creation address with the current block's number.
pub fn run(
    world: &World,
    block: &evm::Context,
    environment: &Environment,
    request: &CallRequest,
    head: u64,
) -> Result<CallResult, CallError> {
    let message = Message {
        from: request.from,
        to: request.to,
        value: request.value,
        gas: request.gas,
        data: request.data.clone(),
        access_list: request.access_list.clone(),
    };
    let tracer = if request.trace_access_list {
        let to = match request.to {
            Some(to) => to,
            None => creation_target(world, request, head, block.location)?,
        };
        Some(AccessListTracer::new(
            &request.access_list,
            request.from,
            to,
            &message::precompiles(block.location),
        ))
    } else {
        None
    };
    let before = tracer.as_ref().map(AccessListTracer::access_list);
    match message::apply(world, block, environment, &message, tracer).map_err(view_error)? {
        Applied::Rejected { error, intrinsic } => Err(CallError::Message {
            text: error,
            intrinsic_gas: intrinsic,
        }),
        Applied::Done(e) => Ok(CallResult {
            used_gas: e.used_gas,
            error: e.error,
            reverted: e.reverted,
            return_data: e.return_data,
            access_list: before.map(|before| TracedAccessList {
                before,
                after: e
                    .tracer
                    .as_ref()
                    .map(AccessListTracer::access_list)
                    .unwrap_or_default(),
            }),
        }),
    }
}

/// The address a creation would get, which `AccessList` excludes:
/// `crypto.CreateAddress`, else `vm.GrindContract` with unlimited gas.
fn creation_target(
    world: &World,
    request: &CallRequest,
    head: u64,
    location: [u8; 2],
) -> Result<Address, CallError> {
    let nonce = world
        .account(&request.from)
        .map_err(view_error)?
        .map_or(0, |a| a.nonce);
    calls::creation_address(
        request.from,
        nonce,
        &request.data,
        None,
        head,
        location,
        u64::MAX,
    )
    .map(|(a, _)| a)
    .map_err(|_| {
        CallError::View(ViewError::Go(format!(
            "exceeded number of attempts grinding address {}",
            message::checksum(&request.from)
        )))
    })
}

/// The block fields of `NewEVMBlockContext`; `message::apply` sets the
/// message fields.
fn context(view: &ExportView, block: &BlockData) -> Result<evm::Context, CallError> {
    let w = block.header.wire();
    let body = block.object.header().wire();
    let parent = view.parent_header(block)?;
    let genesis_parent = block.number == 1;
    let coinbase: [u8; 20] = w
        .primary_coinbase
        .as_ref()
        .and_then(|c| c.value.as_slice().try_into().ok())
        .unwrap_or([0; 20]);
    Ok(evm::Context {
        address: [0; 20],
        caller: [0; 20],
        origin: [0; 20],
        coinbase,
        etx_sender: [0; 20],
        location: view.location(),
        tx_type: 0,
        value: Word::ZERO,
        gas_price: Word::ZERO,
        block_number: Word::from_u64(block.number),
        timestamp: Word::from_u64(parent.header().wire().time.unwrap_or_default()),
        difficulty: from_bytes(w.difficulty.as_deref().unwrap_or_default()),
        base_fee: from_bytes(body.base_fee.as_deref().unwrap_or_default()),
        chain_id: Word::from_u64(view.chain_id()),
        // The parent's state size; the genesis parent counts as zero.
        state_size: if genesis_parent {
            Word::ZERO
        } else {
            from_bytes(
                parent
                    .object()
                    .header()
                    .wire()
                    .quai_state_size
                    .as_deref()
                    .unwrap_or_default(),
            )
        },
        block_gas_limit: body.gas_limit.unwrap_or_default(),
        address_listed: false,
        warm_slots: BTreeSet::new(),
        read_only: false,
        trace: false,
    })
}
fn environment(view: &ExportView, block: &BlockData) -> Result<Environment, CallError> {
    let body = block.object.header().wire();
    let prime_hash: Hash = body
        .prime_terminus_hash
        .as_ref()
        .and_then(|h| h.value.as_slice().try_into().ok())
        .unwrap_or([0; 32]);
    let eligible = view
        .header_by_hash(&prime_hash)?
        .and_then(|r| {
            r.object()
                .header()
                .wire()
                .etx_eligible_slices
                .as_ref()
                .and_then(|h| h.value.as_slice().try_into().ok())
        })
        .ok_or_else(|| CallError::View(ViewError::Unavailable("prime terminus header".into())))?;
    Ok(Environment {
        prime_number: Some(
            block
                .header
                .wire()
                .prime_terminus_number
                .as_deref()
                .map_or(0, |b| {
                    let mut n = [0u8; 8];
                    let take = b.len().min(8);
                    n[8 - take..].copy_from_slice(&b[b.len() - take..]);
                    u64::from_be_bytes(n)
                }),
        ),
        block_hashes: view.ancestor_hashes(block),
        // `types.Message.Hash()` of a call: the zero hash, which an ETX
        // names as its originating transaction.
        transaction_hash: Some([0; 32]),
        eligible_slices: Some(eligible),
        delete_empty: true,
    })
}
