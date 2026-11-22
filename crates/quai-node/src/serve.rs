//! Answers to the peers' `/quai/1.0.0` requests from this node's block
//! store (go-quai p2p/protocol/handler.go `handleRequest`, p2p/node/api.go).
//!
//! The store keeps the zone chain of slice [0 0]: zone blocks by hash or by
//! number (`GetBlockHashByNumber`, then `GetWorkObject`), ten blocks from a
//! hash (`GetWorkObjectsFrom`: the block and the next nine canonical blocks,
//! each the child of the one before, or none), and zone block hashes by
//! number (the genesis hash at 0, which a shadow run's store does not hold).
//! Prime and region views and header views are not kept: those requests get
//! go-quai's "not found" answer, as everything did before.
use crate::rpc_view::ExportView;
use prost::Message;
use quai_network::public::serve::{NotFound, Store};
use quai_primitives::schema::{
    block::{ProtoWorkObject, ProtoWorkObjectBlockView, ProtoWorkObjectBlocksView},
    common::ProtoHash,
    quaiprotocol::{quai_request_message as rq, quai_response_message as rs, QuaiRequestMessage},
};
use quai_primitives::Hash;

/// go-quai's `C_NumPrimeBlocksToDownload`: the blocks of one
/// WorkObjectBlocks answer.
const BATCH: u64 = 10;

pub struct StoreServer {
    view: ExportView,
    genesis: Hash,
    location: Vec<u8>,
}
impl StoreServer {
    pub fn new(view: ExportView, genesis: Hash, location: [u8; 2]) -> Self {
        Self {
            view,
            genesis,
            location: location.to_vec(),
        }
    }
    fn hash_at(&self, number: u64) -> Option<Hash> {
        if number == 0 {
            return Some(self.genesis);
        }
        self.view.hash_at(number)
    }
    fn object(&self, hash: &Hash) -> Option<(u64, ProtoWorkObject)> {
        let (number, bytes) = self.view.raw_block(hash)?;
        Some((number, ProtoWorkObject::decode(bytes.as_slice()).ok()?))
    }
    /// `GetWorkObjectsFrom(hash, location, 10)`.
    fn from(&self, hash: &Hash) -> Option<Vec<ProtoWorkObjectBlockView>> {
        let (number, first) = self.object(hash)?;
        let mut out = vec![ProtoWorkObjectBlockView {
            work_object: Some(first),
        }];
        let mut previous = *hash;
        for i in 1..BATCH {
            let next = self.hash_at(number + i)?;
            let (_, wo) = self.object(&next)?;
            // "The parent hash has to be continuous".
            if zone_parent(&wo)? != previous {
                return None;
            }
            previous = next;
            out.push(ProtoWorkObjectBlockView {
                work_object: Some(wo),
            });
        }
        Some(out)
    }
}
/// The zone parent hash of a work object (its work object header's).
fn zone_parent(wo: &ProtoWorkObject) -> Option<Hash> {
    wo.wo_header
        .as_ref()?
        .parent_hash
        .as_ref()?
        .value
        .as_slice()
        .try_into()
        .ok()
}
/// A `big.Int` request number; None above u64.
fn number(bytes: &[u8]) -> Option<u64> {
    let start = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len());
    let digits = &bytes[start..];
    (digits.len() <= 8).then(|| digits.iter().fold(0u64, |n, b| (n << 8) | *b as u64))
}
impl Store for StoreServer {
    fn answer(&self, request: &QuaiRequestMessage) -> Option<rs::Response> {
        let here = request
            .location
            .as_ref()
            .is_some_and(|l| l.value == self.location);
        if !here {
            return NotFound.answer(request);
        }
        let data = request.data.as_ref()?;
        let named = || match data {
            rq::Data::Hash(h) => h.value.as_slice().try_into().ok(),
            rq::Data::Number(n) => number(n).and_then(|n| self.hash_at(n)),
        };
        match request.request.as_ref()? {
            rq::Request::BlockHash(_) => match data {
                rq::Data::Number(n) => Some(rs::Response::BlockHash(ProtoHash {
                    value: number(n)
                        .and_then(|n| self.hash_at(n))
                        .map(|h| h.to_vec())
                        .unwrap_or_default(),
                })),
                rq::Data::Hash(_) => None,
            },
            rq::Request::WorkObjectBlock(_) => Some(rs::Response::WorkObjectBlockView(
                named()
                    .and_then(|h| self.object(&h))
                    .map(|(_, wo)| ProtoWorkObjectBlockView {
                        work_object: Some(wo),
                    })
                    .unwrap_or_default(),
            )),
            rq::Request::WorkObjectBlocks(_) => Some(rs::Response::WorkObjectBlocksView(
                ProtoWorkObjectBlocksView {
                    work_objects: named().and_then(|h| self.from(&h)).unwrap_or_default(),
                },
            )),
            _ => NotFound.answer(request),
        }
    }
}
