//! Serial zone execution composition. A successful report checks execution
//! commitments, never header/proof/hierarchy admission or canonical publication.
use crate::{
    economics,
    history::{History, Record},
    payouts,
    private_profile::PrivateChain,
    rules::Uint,
    share_rules,
    verifier::Verifier,
    Error,
};
use crypto_bigint::U1024;
use quai_execution::{
    evm::{
        self,
        calls::Environment,
        state::{Account, World},
        word::{from_bytes, Word},
    },
    gas,
    historical::{self, Trimmed, Unlock},
    incoming,
    qi::{self, UtxoView},
    qi_settlement,
    settlement::{self, BlockContext},
    utxo::{self, UtxoState},
};
use quai_primitives::spec::FORKS;
use quai_primitives::{
    receipt::{receipts_root, Receipt},
    transaction::{Kind, Transaction},
    trie,
    work::Context as BodyContext,
    Hash, Header,
};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub world: World,
    pub utxos: UtxoState,
}
pub struct Profile {
    chain_id: Uint,
    location: [u8; 2],
    genesis: BTreeSet<Hash>,
    official_allocations: bool,
}
impl Profile {
    /// Chain ID, slice location, trusted genesis set and the allocation policy
    /// (`true`: the official genesis unlocks of core/state/gen_allocs.go).
    pub fn new(
        chain_id: Uint,
        location: [u8; 2],
        genesis: BTreeSet<Hash>,
        official_allocations: bool,
    ) -> Result<Self, Error> {
        if location.iter().any(|v| *v >= 16) || genesis.is_empty() {
            return Err(Error::Invalid("zone execution profile"));
        }
        Ok(Self {
            chain_id,
            location,
            genesis,
            official_allocations,
        })
    }
    pub fn official(
        chain_id: Uint,
        location: [u8; 2],
        genesis: BTreeSet<Hash>,
    ) -> Result<Self, Error> {
        Self::new(chain_id, location, genesis, true)
    }
    pub fn reviewed_private() -> Result<Self, Error> {
        let p = PrivateChain::reviewed()?;
        Self::from_private(&p)
    }
    pub fn reviewed_admission() -> Result<Self, Error> {
        Self::from_private(&PrivateChain::reviewed_admission()?)
    }
    /// The reviewed private hierarchy chain: empty allocations, as
    /// `execute_prepared_components` requires for it.
    pub fn reviewed_hierarchy() -> Result<Self, Error> {
        Self::from_private(&PrivateChain::reviewed_hierarchy()?)
    }
    pub(crate) fn from_private(p: &PrivateChain) -> Result<Self, Error> {
        Ok(Self {
            chain_id: Uint::from_u64(p.chain_id),
            location: p.location,
            genesis: BTreeSet::from([p.genesis[2].id()]),
            official_allocations: false,
        })
    }
}
pub struct Executed {
    pub receipts: Vec<Receipt>,
    /// The index in the block body of the transaction behind each receipt
    /// (an incoming ETX can leave no receipt).
    pub receipt_txs: Vec<u32>,
    pub outbound: Vec<Transaction>,
    pub unlocks: Vec<Unlock>,
    pub trimmed: Vec<Trimmed>,
    pub created_keys: Vec<(qi::OutPoint, u8)>,
    pub deleted_hashes: Vec<Hash>,
    pub gas_used: u64,
    pub state_used: u64,
    pub total_etx_gas: u64,
    pub quai_fees: Uint,
    pub qi_fees: Uint,
    pub supply_added: Uint,
    pub supply_removed: Uint,
    pub fully_validated: bool,
}
fn state_error(error: quai_execution::state_error::ReadError, fallback: Error) -> Error {
    match error {
        quai_execution::state_error::ReadError::Unavailable(_) => {
            Error::Unavailable("account/storage backend")
        }
        _ => fallback,
    }
}
fn invalid(name: &'static str) -> Error {
    Error::Invalid(name)
}
fn execution(error: settlement::Error) -> Error {
    match error {
        settlement::Error::Invalid(name) => Error::Invalid(name),
        settlement::Error::Unavailable(_) => Error::Unavailable("execution state backend"),
        settlement::Error::Unsupported(_) => Error::Unsupported("execution capability domain"),
    }
}
fn uint(v: &Option<Vec<u8>>) -> Result<Uint, Error> {
    share_rules::uint(v.as_deref().ok_or(invalid("missing execution integer"))?)
}
fn hash(v: &Option<quai_primitives::schema::common::ProtoHash>) -> Hash {
    v.as_ref().unwrap().value.as_slice().try_into().unwrap()
}
fn add(a: Uint, b: Uint) -> Result<Uint, Error> {
    let n: U1024 = a.resize();
    let n = n.wrapping_add(&b.resize());
    if n.bits_vartime() > 256 {
        return Err(Error::Unsupported("execution sum magnitude"));
    }
    Ok(n.resize())
}
fn budgets(
    out: &[Transaction],
    location: [u8; 2],
    region: &mut u64,
    prime: &mut u64,
) -> Result<(), Error> {
    for tx in out {
        let Kind::External(etx) = tx.kind() else {
            return Err(invalid("outbound transaction type"));
        };
        if etx.to[0] == location[0] << 4 | location[1] {
            continue;
        }
        let budget = if etx.to[0] >> 4 == location[0] {
            &mut *region
        } else {
            &mut *prime
        };
        *budget = budget
            .checked_sub(etx.gas)
            .ok_or(invalid("outbound ETX budget"))?;
    }
    Ok(())
}
fn inner<const OFFICIAL_ALLOCATIONS: bool, F: FnMut(&Header) -> Result<Hash, Error>>(
    state: &mut State,
    history: &History,
    past: &impl historical::History,
    block: &Record,
    profile: &Profile,
    inbound: &[Transaction],
    verify_commitments: bool,
    work: F,
) -> Result<Executed, Error> {
    let body = block.object.header().wire();
    let wh = block.header.wire();
    if wh.location.as_ref().unwrap().value != profile.location {
        return Err(invalid("execution location"));
    }
    block
        .object
        .verify_commitments(BodyContext::Zone)
        .map_err(|_| invalid("block body commitments"))?;
    let parent = history.get(&block.parents[2])?;
    if parent.numbers[2].checked_add(1) != Some(block.numbers[2]) {
        return Err(invalid("execution parent height"));
    }
    let genesis = profile.genesis.contains(&parent.id);
    let empty = trie::secure_root(&[]).map_err(|_| invalid("empty root"))?;
    if state
        .world
        .root()
        .map_err(|error| state_error(error, Error::ResourceLimit("parent account state")))?
        != if genesis {
            empty
        } else {
            hash(&parent.object.header().wire().evm_root)
        }
        || state
            .world
            .etxs()
            .root()
            .map_err(|error| state_error(error, Error::ResourceLimit("parent ETX state")))?
            != if genesis {
                empty
            } else {
                hash(&parent.object.header().wire().etx_set_root)
            }
        || if genesis {
            !state.utxos.is_empty()
                || !state
                    .world
                    .lockups_is_empty()
                    .map_err(|e| state_error(e, Error::ResourceLimit("genesis lockups")))?
        } else {
            state.utxos.root() != hash(&parent.object.header().wire().utxo_root)
        }
    {
        return Err(invalid("execution parent state binding"));
    }
    let prime = history.get(&hash(&body.prime_terminus_hash))?;
    let exchange = uint(&prime.object.header().wire().exchange_rate)?;
    let quote = economics::reward_quote(&block.header, exchange, uint(&body.avg_tx_fees)?)?;
    let eligible = hash(&prime.object.header().wire().etx_eligible_slices);
    let parent_size = if genesis {
        Uint::ZERO
    } else {
        uint(&parent.object.header().wire().quai_state_size)?
    };
    let gas_limit = body.gas_limit.unwrap();
    let base_fee = uint(&body.base_fee)?;
    let mut block_hashes = BTreeMap::new();
    let mut cursor = parent.clone();
    for _ in 0..256 {
        block_hashes.insert(cursor.numbers[2], cursor.id);
        if cursor.numbers[2] == 0 {
            break;
        }
        match history.get(&cursor.parents[2]) {
            Ok(v) => cursor = v,
            Err(_) => break,
        }
    }
    let mut c = incoming::Context {
        block: BlockContext {
            vm: evm::Context {
                address: [0; 20],
                caller: [0; 20],
                origin: [0; 20],
                coinbase: wh
                    .primary_coinbase
                    .as_ref()
                    .unwrap()
                    .value
                    .as_slice()
                    .try_into()
                    .unwrap(),
                etx_sender: [0; 20],
                location: profile.location,
                tx_type: 0,
                value: Word::ZERO,
                gas_price: Word::ZERO,
                block_number: Uint::from_u64(block.numbers[2]),
                timestamp: Uint::from_u64(parent.header.wire().time.unwrap()),
                difficulty: block.difficulty()?,
                base_fee,
                chain_id: profile.chain_id,
                state_size: parent_size,
                block_gas_limit: gas_limit,
                address_listed: false,
                warm_slots: BTreeSet::new(),
                read_only: false,
                trace: false,
            },
            environment: Environment {
                prime_number: Some(block.prime),
                eligible_slices: Some(eligible),
                block_hashes,
                delete_empty: true,
                ..Default::default()
            },
            gas_remaining: gas_limit,
            cumulative_gas: 0,
            state_used: 0,
        },
        parent_height: parent.numbers[2],
        total_etx_gas: 0,
        region_remaining: ((parent.object.transactions().len() as u64).wrapping_mul(21000))
            .max(1050000),
        prime_remaining: ((parent.object.transactions().len() as u64).wrapping_mul(21000))
            .max(1050000),
    };
    let base_utxos = state.utxos.clone();
    let scaling = (state.utxos.consensus_size() as f64).ln();
    state.world.begin_block();
    state
        .world
        .etxs_mut()
        .push(inbound)
        .map_err(|error| state_error(error, Error::ResourceLimit("inbound ETX queue")))?;
    let unlocks = historical::redeem(
        &mut state.world,
        past,
        block.numbers[2],
        parent_size,
        profile.location,
    )
    .map_err(execution)?;
    let mut receipts = vec![];
    let mut outbound = vec![];
    let mut created_keys = vec![];
    let mut deleted_hashes = vec![];
    let mut quai_fees = Uint::ZERO;
    let mut qi_fees = Uint::ZERO;
    let mut supply_added = Uint::ZERO;
    let mut supply_removed = Uint::ZERO;
    let mut previous_price = Uint::ZERO;
    let mut previous_external = true;
    let mut first_qi = true;
    let mut etx_count = 0u64;
    let mut receipt_txs = vec![];
    for (tx_index, tx) in block.object.transactions().iter().enumerate() {
        let mut price = None;
        let receipts_before = receipts.len();
        match tx.kind() {
            Kind::Quai(q) => {
                let v = settlement::apply_quai(&mut state.world, tx, &mut c.block)
                    .map_err(execution)?;
                quai_fees = add(quai_fees, v.fee)?;
                if v.receipt.status == 1 {
                    budgets(
                        &v.receipt.outbound_etxs,
                        profile.location,
                        &mut c.region_remaining,
                        &mut c.prime_remaining,
                    )?;
                    for h in v.auxiliary.deleted_hashes {
                        state.utxos.remove_auxiliary_hash(h);
                        deleted_hashes.push(h);
                    }
                    outbound.extend(v.receipt.outbound_etxs.iter().cloned());
                }
                price = Some(from_bytes(&q.gas_price));
                receipts.push(v.receipt);
            }
            Kind::Qi(qi_tx) => {
                let mut qi_ctx = qi_settlement::Context {
                    inputs: qi::Context {
                        chain_id: quai_execution::economic_state::magnitude(profile.chain_id),
                        block_height: block.numbers[2],
                        location: profile.location,
                    },
                    prime_number: block.prime,
                    scaling_factor: scaling,
                    base_fee,
                    quai_conversion_reward: quote.conversion_quai,
                    qi_reward: quote.qi,
                    eligible_slices: eligible,
                    first_qi,
                    block_gas_limit: gas_limit,
                    gas_remaining: c.block.gas_remaining,
                    used_gas: c.block.cumulative_gas,
                    region_etx_remaining: c.region_remaining,
                    prime_etx_remaining: c.prime_remaining,
                };
                let spend_hashes = qi_tx
                    .inputs
                    .iter()
                    .map(|input| {
                        let point = qi::OutPoint {
                            hash: input.hash,
                            index: input.index,
                        };
                        let value = state
                            .utxos
                            .get(&point)
                            .map_err(|e| match e {
                                qi::ReadError::Unavailable(_) => {
                                    Error::Unavailable("Qi state backend")
                                }
                                _ => invalid("Qi spent record"),
                            })?
                            .ok_or(invalid("Qi spent record"))?;
                        Ok(utxo::utxo_hash(point, &value))
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                let v = qi_settlement::apply_qi(&mut state.utxos, tx, &mut qi_ctx)
                    .map_err(execution)?;
                deleted_hashes.extend(spend_hashes);
                created_keys.extend(v.created.iter().map(|(p, u)| (*p, u.denomination)));
                qi_fees = add(qi_fees, Uint::from_u64(v.fee_qit))?;
                supply_added = add(supply_added, Uint::from_u64(v.added_qit))?;
                supply_removed = add(supply_removed, Uint::from_u64(v.removed_qit))?;
                let fee = payouts::convert_qi(&quote, Uint::from_u64(v.fee_qit))?;
                // ProcessQiTx refused shorter output addresses above.
                let block_gas = gas::block_qi_gas(tx, scaling, profile.location)
                    .map_err(|_| Error::Unsupported("Qi price gas domain"))?;
                price = Some(fee.wrapping_div(&Uint::from_u64(block_gas)));
                c.block.gas_remaining = qi_ctx.gas_remaining;
                c.block.cumulative_gas = qi_ctx.used_gas;
                c.region_remaining = qi_ctx.region_etx_remaining;
                c.prime_remaining = qi_ctx.prime_etx_remaining;
                first_qi = false;
                outbound.extend(v.outbound);
                receipts.push(v.receipt);
            }
            Kind::External(_) => {
                let v = incoming::apply(&mut state.world, &mut state.utxos, tx, &mut c)
                    .map_err(execution)?;
                supply_added = add(supply_added, v.supply_added)?;
                created_keys.extend(v.created);
                deleted_hashes.extend(v.deleted_hashes);
                outbound.extend(v.outbound);
                receipts.extend(v.receipt.map(|r| r.receipt));
                etx_count += 1;
            }
        }
        receipt_txs.resize(receipts.len(), tx_index as u32);
        debug_assert!(receipts.len() <= receipts_before + 1);
        if let Some(price) = price {
            if price < base_fee {
                return Err(invalid("transaction price below base fee"));
            }
            if !previous_external && price > previous_price {
                return Err(invalid("transaction fee ordering"));
            }
            previous_price = price;
            previous_external = false;
        } else {
            previous_external = true;
        }
    }
    let available = state
        .world
        .etxs()
        .read(state.world.etxs().oldest())
        .map_err(|error| state_error(error, Error::ResourceLimit("ETX head lookup")))?
        .is_some();
    if block.numbers[2] <= FORKS.start_tx {
        if available && etx_count < 50 || etx_count > 100 {
            return Err(invalid("ETX count range"));
        }
    } else if available && c.total_etx_gas < gas_limit / 5
        || c.total_etx_gas > (gas_limit / 5).wrapping_mul(2)
    {
        return Err(invalid("ETX gas range"));
    }
    let expected_avg = payouts::average_fees(block, &parent, genesis, &quote, quai_fees, qi_fees)?;
    let total_fees = add(quai_fees, payouts::convert_qi(&quote, qi_fees)?)?;
    if expected_avg != uint(&body.avg_tx_fees)? || total_fees != uint(&body.total_fees)? {
        return Err(invalid("block fee accounting"));
    }
    let payouts = payouts::project(
        history,
        block,
        profile.location,
        exchange,
        outbound.len(),
        work,
    )?;
    outbound.extend(payouts.outbound);
    if OFFICIAL_ALLOCATIONS {
        historical::allocate(
            &mut state.world,
            block.numbers[2],
            block.prime,
            profile.location,
        )
        .map_err(execution)?;
    }
    if genesis {
        let mut address = [0; 20];
        address[0] = profile.location[0] << 4 | profile.location[1];
        address[19] = 10;
        let mut account = state
            .world
            .account(&address)
            .map_err(|e| state_error(e, Error::ResourceLimit("genesis lockup account")))?
            .map(|v| (*v).clone())
            .unwrap_or_else(Account::default);
        account.nonce = 1;
        state
            .world
            .insert(address, account)
            .map_err(|error| state_error(error, Error::ResourceLimit("genesis lockup account")))?;
    }
    if state
        .utxos
        .consensus_size()
        .wrapping_add(deleted_hashes.len() as u64)
        < deleted_hashes.len() as u64
    {
        return Err(invalid("UTXO count underflow before trimming"));
    }
    let (trimmed, removed) = historical::trim(
        &mut state.utxos,
        &base_utxos,
        past,
        block.numbers[2],
        block.prime,
        &deleted_hashes,
    )
    .map_err(execution)?;
    supply_removed = add(supply_removed, removed)?;
    state
        .world
        .finish_block()
        .map_err(|error| state_error(error, Error::Unsupported("block account finalization")))?;
    created_keys.sort_by_key(|(_, denom)| *denom);
    if verify_commitments {
        let checks =
            [
                (
                    c.block.cumulative_gas == body.gas_used.unwrap(),
                    "execution gas counter",
                ),
                (
                    c.block.state_used == body.state_used.unwrap(),
                    "execution state counter",
                ),
                (
                    state.world.root().map_err(|error| {
                        state_error(error, Error::ResourceLimit("account root"))
                    })? == hash(&body.evm_root),
                    "execution account root",
                ),
                (
                    state
                        .world
                        .etxs()
                        .root()
                        .map_err(|error| state_error(error, Error::ResourceLimit("ETX root")))?
                        == hash(&body.etx_set_root),
                    "execution ETX root",
                ),
                (
                    state.utxos.root() == hash(&body.utxo_root),
                    "execution UTXO root",
                ),
                (
                    Uint::from_u64(state.world.account_count()) == uint(&body.quai_state_size)?,
                    "execution account count",
                ),
                (
                    receipts_root(&receipts).map_err(|_| invalid("receipt encoding"))?
                        == hash(&body.receipt_hash),
                    "execution receipt root",
                ),
                (
                    quai_primitives::work::transactions_root(&outbound)
                        .map_err(|_| invalid("outbound encoding"))?
                        == hash(&body.outbound_etx_hash),
                    "execution outbound root",
                ),
            ];
        for (valid, name) in checks {
            if !valid {
                return Err(invalid(name));
            }
        }
    }
    Ok(Executed {
        receipts,
        receipt_txs,
        outbound,
        unlocks,
        trimmed,
        created_keys,
        deleted_hashes,
        gas_used: c.block.cumulative_gas,
        state_used: c.block.state_used,
        total_etx_gas: c.total_etx_gas,
        quai_fees,
        qi_fees,
        supply_added,
        supply_removed,
        fully_validated: false,
    })
}
/// Atomic execution projection for offline oracle qualification. It cannot mint
/// an admitted-block token, publish state, or enable a network runtime mode.
fn project_with_allocations<
    const OFFICIAL_ALLOCATIONS: bool,
    F: FnMut(&Header) -> Result<Hash, Error>,
>(
    state: &mut State,
    history: &History,
    past: &impl historical::History,
    block: &Record,
    profile: &Profile,
    inbound: &[Transaction],
    work: F,
) -> Result<Executed, Error> {
    let mut staged = state.clone();
    let result = inner::<OFFICIAL_ALLOCATIONS, _>(
        &mut staged,
        history,
        past,
        block,
        profile,
        inbound,
        true,
        work,
    )?;
    *state = staged;
    Ok(result)
}
pub fn project<F: FnMut(&Header) -> Result<Hash, Error>>(
    state: &mut State,
    history: &History,
    past: &impl historical::History,
    block: &Record,
    profile: &Profile,
    inbound: &[Transaction],
    work: F,
) -> Result<Executed, Error> {
    if profile.official_allocations {
        project_with_allocations::<true, _>(state, history, past, block, profile, inbound, work)
    } else {
        project_with_allocations::<false, _>(state, history, past, block, profile, inbound, work)
    }
}
/// The values one block execution computed, for reporting which header
/// commitment differs after `project` refused a block.
#[derive(Clone)]
pub struct Computed {
    pub gas_used: u64,
    pub state_used: u64,
    pub evm_root: Hash,
    pub etx_root: Hash,
    pub utxo_root: Hash,
    pub account_count: u64,
    pub receipt_root: Hash,
    pub outbound_root: Hash,
    pub receipts: Vec<Receipt>,
    pub outbound: Vec<Transaction>,
}
/// Execute a block on a copy of `state` without the final commitment
/// comparison and return what it computed. Diagnostics only: the caller's
/// state is unchanged, and the result admits nothing.
pub fn diagnose<F: FnMut(&Header) -> Result<Hash, Error>>(
    state: &State,
    history: &History,
    past: &impl historical::History,
    block: &Record,
    profile: &Profile,
    inbound: &[Transaction],
    work: F,
) -> Result<Computed, Error> {
    let mut staged = state.clone();
    let executed = if profile.official_allocations {
        inner::<true, _>(
            &mut staged,
            history,
            past,
            block,
            profile,
            inbound,
            false,
            work,
        )?
    } else {
        inner::<false, _>(
            &mut staged,
            history,
            past,
            block,
            profile,
            inbound,
            false,
            work,
        )?
    };
    let root = |r: Result<Hash, quai_execution::state_error::ReadError>, what| {
        r.map_err(|e| state_error(e, Error::ResourceLimit(what)))
    };
    Ok(Computed {
        gas_used: executed.gas_used,
        state_used: executed.state_used,
        evm_root: root(staged.world.root(), "account root")?,
        etx_root: root(staged.world.etxs().root(), "ETX root")?,
        utxo_root: staged.utxos.root(),
        account_count: staged.world.account_count(),
        receipt_root: receipts_root(&executed.receipts).map_err(|_| invalid("receipt encoding"))?,
        outbound_root: quai_primitives::work::transactions_root(&executed.outbound)
            .map_err(|_| invalid("outbound encoding"))?,
        receipts: executed.receipts,
        outbound: executed.outbound,
    })
}
pub fn native_components(
    verifier: &mut Verifier,
    state: &mut State,
    history: &History,
    past: &impl historical::History,
    block: &Record,
    profile: &Profile,
    inbound: &[Transaction],
) -> Result<Executed, Error> {
    project(state, history, past, block, profile, inbound, |h| {
        verifier
            .work_for_inclusion(h, &profile.location)
            .map(|v| v.hash())
    })
}

/// Reviewed private chains have an explicit empty-allocation policy. Select
/// that policy at compile time so their node binary does not retain the official
/// allocation table. Public official execution keeps the other specialization.
pub(crate) fn native_private_components(
    verifier: &mut Verifier,
    state: &mut State,
    history: &History,
    past: &impl historical::History,
    block: &Record,
    profile: &Profile,
    inbound: &[Transaction],
) -> Result<Executed, Error> {
    if profile.official_allocations {
        return Err(Error::Invalid("private allocation policy"));
    }
    project_with_allocations::<false, _>(state, history, past, block, profile, inbound, |h| {
        verifier
            .work_for_inclusion(h, &profile.location)
            .map(|v| v.hash())
    })
}

/// Atomic header/body/manifest/PCRC and execution checks on one local-order
/// zone branch. Ancestry and prior confirmed inbounds are caller-authenticated
/// inputs. The result cannot publish a canonical generation or grant authority.
pub struct LocalComponents {
    pub append: crate::placement::Prepared,
    pub execution: Executed,
}
pub fn inspect_local_components(
    verifier: &mut Verifier,
    state: &mut State,
    history: &History,
    past: &impl historical::History,
    index: &crate::placement::CandidateIndex,
    block: &Record,
    profile: &Profile,
    placement_profile: &crate::placement::Profile,
    prior_confirmed: &[Transaction],
    now: u64,
) -> Result<LocalComponents, Error> {
    if placement_profile.location != profile.location
        || placement_profile.genesis != profile.genesis
    {
        return Err(Error::Invalid("append/execution profile binding"));
    }
    let append = crate::zone_admission::inspect_indexed_local(
        verifier,
        history,
        index,
        block,
        placement_profile,
        now,
    )?;
    let execution = native_components(
        verifier,
        state,
        history,
        past,
        block,
        profile,
        prior_confirmed,
    )?;
    Ok(LocalComponents { append, execution })
}

/// Execute the exact block prepared by the private hierarchy, taking prior
/// confirmed inbounds only from that preparation. Raw caller lists cannot
/// replace the checked derivation. State still commits atomically, and this
/// component result cannot publish an admitted canonical generation.
pub fn execute_prepared_components(
    verifier: &mut Verifier,
    state: &mut State,
    history: &History,
    past: &impl historical::History,
    block: &Record,
    profile: &Profile,
    prepared: &crate::confirmation::PreparedConfirmation,
) -> Result<Executed, Error> {
    if !prepared.binds(block) {
        return Err(Error::Invalid("hierarchy/execution block binding"));
    }
    let expected = prepared.chain_profile();
    let official = expected.spec().allocations == quai_primitives::spec::Allocations::Official;
    if profile.official_allocations != official
        || profile.chain_id != Uint::from_u64(expected.spec().chain_id)
        || profile.location != expected.location()
        || profile.genesis != BTreeSet::from([expected.genesis_hash()])
    {
        return Err(Error::Invalid("hierarchy/execution profile binding"));
    }
    if official {
        native_components(
            verifier,
            state,
            history,
            past,
            block,
            profile,
            prepared.inbound_for_execution(),
        )
    } else {
        native_private_components(
            verifier,
            state,
            history,
            past,
            block,
            profile,
            prepared.inbound_for_execution(),
        )
    }
}
