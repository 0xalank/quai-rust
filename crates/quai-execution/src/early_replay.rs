//! Whole-work-object execution for the first three empty-allocation private
//! blocks. Uses actual transaction settlement and verifies execution commitments.
//! This is an execution replay component, never proof/hierarchy admission.
use crate::{
    evm::{
        self,
        calls::Environment,
        state::World,
        word::{from_bytes, uint, Word},
    },
    muhash::MuHash,
    settlement::{self, BlockContext, Error},
};
use quai_primitives::{
    receipt::{receipts_root, Receipt},
    transaction::Kind,
    work::{Context as BodyContext, WorkObject},
    Hash,
};
use std::collections::BTreeSet;
#[derive(Clone)]
pub struct EarlyReplay {
    world: World,
    genesis: Hash,
    head: Hash,
    height: u64,
    time: u64,
}
pub struct Replayed {
    pub receipts: Vec<Receipt>,
    pub gas_used: u64,
    pub state_used: u64,
    pub evm_root: Hash,
    pub etx_root: Hash,
    pub utxo_root: Hash,
}
fn invalid(s: &'static str) -> Error {
    Error::Invalid(s)
}
fn unsupported(s: &'static str) -> Error {
    Error::Unsupported(evm::CapabilityError::Context(s))
}
fn hash(v: &Option<quai_primitives::schema::common::ProtoHash>) -> Hash {
    v.as_ref().unwrap().value.as_slice().try_into().unwrap()
}
impl EarlyReplay {
    /// A caller-provided private genesis anchor is not a consensus certificate.
    pub fn empty(genesis: Hash, timestamp: u64) -> Self {
        Self {
            world: World::default(),
            genesis,
            head: genesis,
            height: 0,
            time: timestamp,
        }
    }
    pub fn world(&self) -> &World {
        &self.world
    }
    pub fn head(&self) -> Hash {
        self.head
    }
    pub fn height(&self) -> u64 {
        self.height
    }
    pub fn genesis(&self) -> Hash {
        self.genesis
    }
    pub fn timestamp(&self) -> u64 {
        self.time
    }
    pub fn restore_local(
        world: World,
        genesis: Hash,
        head: Hash,
        height: u64,
        timestamp: u64,
    ) -> Result<Self, Error> {
        world
            .validate()
            .map_err(|_| unsupported("private checkpoint bounds"))?;
        if world.in_block() || !world.lockups_is_empty()? || !world.etxs().is_pristine()? {
            return Err(unsupported(
                "early checkpoint cannot encode auxiliary or unfinished state",
            ));
        }
        if height > 3 {
            return Err(unsupported("early replay checkpoint height"));
        }
        if height == 0 && (world.account_count() != 0 || head != genesis) {
            return Err(invalid("private empty genesis checkpoint"));
        }
        Ok(Self {
            world,
            genesis,
            head,
            height,
            time: timestamp,
        })
    }
    pub fn apply(&mut self, object: &WorkObject) -> Result<Replayed, Error> {
        object
            .verify_commitments(BodyContext::Zone)
            .map_err(|_| invalid("block body commitments"))?;
        let wh = object
            .work_header(quai_primitives::Schedule {
                kawpow_fork: 1_171_500,
                transition_period: 129600,
            })
            .map_err(|_| invalid("work header"))?;
        let h = wh.wire();
        let body = object.header().wire();
        let number =
            uint(from_bytes(h.number.as_deref().unwrap())).ok_or(invalid("height width"))?;
        if number != self.height + 1
            || h.parent_hash.as_ref().unwrap().value.as_slice() != self.head
        {
            return Err(invalid("private replay parent"));
        }
        if number > 3
            || from_bytes(h.prime_terminus_number.as_deref().unwrap()) != Word::ZERO
            || h.location.as_ref().unwrap().value != [0, 0]
            || !object
                .uncle_headers(quai_primitives::Schedule {
                    kawpow_fork: 1_171_500,
                    transition_period: 129600,
                })
                .map_err(|_| invalid("uncle encoding"))?
                .is_empty()
        {
            return Err(unsupported(
                "early private replay requires heights 1-3, prime zero, zone 0-0 and no uncles",
            ));
        }
        if !object.outbound_etxs().is_empty()
            || object
                .transactions()
                .iter()
                .any(|t| !matches!(t.kind(), Kind::Quai(_)))
        {
            return Err(unsupported("early replay incoming/Qi/reward execution"));
        }
        if from_bytes(body.base_fee.as_deref().unwrap()) != Word::ZERO
            || from_bytes(body.total_fees.as_deref().unwrap()) != Word::ZERO
            || from_bytes(body.avg_tx_fees.as_deref().unwrap()) != Word::ZERO
        {
            return Err(unsupported(
                "early zero-fee empty-allocation private blocks",
            ));
        }
        let gas_limit = body.gas_limit.unwrap();
        let state_limit = body.state_limit.unwrap();
        let mut block = BlockContext {
            vm: evm::Context {
                address: [0; 20],
                caller: [0; 20],
                origin: [0; 20],
                coinbase: h
                    .primary_coinbase
                    .as_ref()
                    .unwrap()
                    .value
                    .as_slice()
                    .try_into()
                    .unwrap(),
                etx_sender: [0; 20],
                location: [0, 0],
                tx_type: 0,
                value: Word::ZERO,
                gas_price: Word::ZERO,
                block_number: Word::from_u64(number),
                timestamp: Word::from_u64(self.time),
                difficulty: from_bytes(h.difficulty.as_deref().unwrap()),
                base_fee: Word::ZERO,
                chain_id: Word::from_u64(1337),
                state_size: Word::from_u64(self.world.account_count()),
                block_gas_limit: gas_limit,
                address_listed: false,
                warm_slots: BTreeSet::new(),
                read_only: false,
                trace: false,
            },
            environment: Environment {
                prime_number: Some(0),
                eligible_slices: Some([0; 32]),
                delete_empty: true,
                block_hashes: std::collections::BTreeMap::from([(self.height, self.head)]),
                ..Default::default()
            },
            gas_remaining: gas_limit,
            cumulative_gas: 0,
            state_used: 0,
        };
        let mut staged = self.world.clone();
        let mut receipts = vec![];
        for tx in object.transactions() {
            let Kind::Quai(q) = tx.kind() else {
                unreachable!()
            };
            if from_bytes(&q.gas_price) != Word::ZERO {
                return Err(unsupported("early zero fee transaction"));
            }
            let out = settlement::apply_quai(&mut staged, tx, &mut block)?;
            if !out.receipt.outbound_etxs.is_empty() {
                return Err(unsupported("early outbound ETX lifecycle"));
            }
            receipts.push(out.receipt);
        }
        if number == 1 {
            let mut a = [0; 20];
            a[19] = 10;
            let mut value = staged
                .account(&a)?
                .map(|v| (*v).clone())
                .unwrap_or_default();
            value.nonce = 1;
            staged
                .insert(a, value)
                .map_err(|_| unsupported("lockup genesis account"))?;
        }
        let evm_root = staged
            .root()
            .map_err(|_| unsupported("private replay account root"))?;
        let etx_root =
            quai_primitives::trie::secure_root(&[]).map_err(|_| invalid("empty ETX root"))?;
        let utxo_root = MuHash::default().hash();
        if evm_root != hash(&body.evm_root)
            || etx_root != hash(&body.etx_set_root)
            || utxo_root != hash(&body.utxo_root)
        {
            return Err(invalid("block state commitments"));
        }
        if receipts_root(&receipts).map_err(|_| unsupported("block receipt bounds"))?
            != hash(&body.receipt_hash)
        {
            return Err(invalid("block receipt commitment"));
        }
        if block.cumulative_gas != body.gas_used.unwrap()
            || block.state_used != body.state_used.unwrap()
            || block.cumulative_gas > gas_limit
            || block.state_used > state_limit
        {
            return Err(invalid("block execution counters"));
        }
        if from_bytes(body.quai_state_size.as_deref().unwrap())
            != Word::from_u64(staged.account_count())
        {
            return Err(invalid("block Quai state size"));
        }
        let id = wh.block_hash().map_err(|_| invalid("block identity"))?;
        self.world = staged;
        self.height = number;
        self.head = id;
        self.time = h.time.unwrap();
        Ok(Replayed {
            receipts,
            gas_used: block.cumulative_gas,
            state_used: block.state_used,
            evm_root,
            etx_root,
            utxo_root,
        })
    }
}

#[cfg(test)]
mod checkpoint_scope_tests {
    use super::*;
    #[test]
    fn early_checkpoint_restore_rejects_state_it_cannot_serialize() {
        let mut world = World::default();
        let mut key = vec![0; 32];
        key[31] = 250;
        world.etxs_mut().set_auxiliary(key, vec![1]).unwrap();
        assert!(EarlyReplay::restore_local(world, [0; 32], [1; 32], 1, 0).is_err());
        let mut world = World::default();
        world
            .set_lockup(
                crate::lockup::Key {
                    owner: [0; 20],
                    beneficiary: [1; 20],
                    lock: 1,
                    epoch: 1,
                },
                Some(crate::lockup::Lockup {
                    balance: Word::ONE,
                    unlock_height: 1,
                    elements: 1,
                    delegate: [0; 20],
                }),
            )
            .unwrap();
        assert!(EarlyReplay::restore_local(world, [0; 32], [1; 32], 1, 0).is_err());
        let mut world = World::default();
        world.begin_block();
        assert!(EarlyReplay::restore_local(world, [0; 32], [1; 32], 1, 0).is_err());
    }
}
