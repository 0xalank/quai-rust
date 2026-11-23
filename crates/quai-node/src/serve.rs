//! Answers to the peers' `/quai/1.0.0` requests from this node's block
//! store (go-quai p2p/protocol/handler.go `handleRequest`, p2p/node/api.go).
//!
//! The store keeps slice [0 0]'s zone chain and the prime and region views
//! of its coincident blocks, so it answers in the three locations of the
//! slice: a block hash by number (`GetBlockHashByNumber`; the genesis hash
//! at 0, which a shadow run's store does not hold), a block by hash or by
//! number (`GetWorkObject`), a header view (`ConvertToHeaderView`: the block
//! with its transactions, manifest and interlink hashes emptied), and ten
//! blocks from a hash (`GetWorkObjectsFrom`: the block and the next nine
//! canonical blocks of that context, each the child of the one before, or
//! none). A block it does not hold, or another location, gets go-quai's
//! "not found" answer.
use crate::rpc_view::ExportView;
use prost::Message;
use quai_network::public::serve::{NotFound, Store};
use quai_primitives::schema::{
    block::{
        ProtoManifest, ProtoTransactions, ProtoWorkObject, ProtoWorkObjectBlockView,
        ProtoWorkObjectBlocksView, ProtoWorkObjectHeaderView,
    },
    common::{ProtoHash, ProtoHashes},
    quaiprotocol::{quai_request_message as rq, quai_response_message as rs, QuaiRequestMessage},
};
use quai_primitives::Hash;

/// go-quai's `C_NumPrimeBlocksToDownload`: the blocks of one
/// WorkObjectBlocks answer.
const BATCH: u64 = 10;
const ZONE: usize = 2;

pub struct StoreServer {
    view: ExportView,
    genesis: Hash,
    /// The locations of the slice's prime, region and zone chains.
    locations: [Vec<u8>; 3],
}
impl StoreServer {
    pub fn new(view: ExportView, genesis: Hash, location: [u8; 2]) -> Self {
        Self {
            view,
            genesis,
            locations: [vec![], vec![location[0]], location.to_vec()],
        }
    }
    fn hash_at(&self, ctx: usize, number: u64) -> Option<Hash> {
        if number == 0 {
            return Some(self.genesis);
        }
        if ctx == ZONE {
            self.view.hash_at(number)
        } else {
            self.view.dominant_hash_at(ctx as u8, number)
        }
    }
    fn object(&self, ctx: usize, hash: &Hash) -> Option<ProtoWorkObject> {
        let bytes = if ctx == ZONE {
            self.view.raw_block(hash)?.1
        } else {
            self.view.raw_dominant(ctx as u8, hash)?
        };
        ProtoWorkObject::decode(bytes.as_slice()).ok()
    }
    /// `GetWorkObjectsFrom(hash, location, 10)`.
    fn from(&self, ctx: usize, hash: &Hash) -> Option<Vec<ProtoWorkObjectBlockView>> {
        let first = self.object(ctx, hash)?;
        let number = number_in(&first, ctx)?;
        let mut out = vec![ProtoWorkObjectBlockView {
            work_object: Some(first),
        }];
        let mut previous = *hash;
        for i in 1..BATCH {
            let height = number.checked_add(i)?;
            let next = self.hash_at(ctx, height)?;
            let wo = self.object(ctx, &next)?;
            // "The parent hash has to be continuous".
            if parent_in(&wo, ctx)? != previous {
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
fn hash32(h: &ProtoHash) -> Option<Hash> {
    h.value.as_slice().try_into().ok()
}
/// `WorkObject.ParentHash(ctx)`: the work object header's in the zone, the
/// body header's parent hash of that context in prime and region.
fn parent_in(wo: &ProtoWorkObject, ctx: usize) -> Option<Hash> {
    if ctx == ZONE {
        return hash32(wo.wo_header.as_ref()?.parent_hash.as_ref()?);
    }
    hash32(wo.wo_body.as_ref()?.header.as_ref()?.parent_hash.get(ctx)?)
}
/// `WorkObject.NumberU64(ctx)`, likewise.
fn number_in(wo: &ProtoWorkObject, ctx: usize) -> Option<u64> {
    let bytes = if ctx == ZONE {
        wo.wo_header.as_ref()?.number.as_deref()?
    } else {
        wo.wo_body.as_ref()?.header.as_ref()?.number.get(ctx)?
    };
    number(bytes)
}
/// `ConvertToHeaderView`: no transactions, manifest or interlink hashes;
/// go-quai encodes the three as present and empty.
pub fn header_view(mut wo: ProtoWorkObject) -> ProtoWorkObject {
    if let Some(body) = wo.wo_body.as_mut() {
        body.transactions = Some(ProtoTransactions::default());
        body.manifest = Some(ProtoManifest::default());
        body.interlink_hashes = Some(ProtoHashes::default());
    }
    wo
}
/// A `big.Int` number; None above u64.
fn number(bytes: &[u8]) -> Option<u64> {
    let start = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len());
    let digits = &bytes[start..];
    (digits.len() <= 8).then(|| digits.iter().fold(0u64, |n, b| (n << 8) | *b as u64))
}
impl Store for StoreServer {
    fn answer(&self, request: &QuaiRequestMessage) -> Option<rs::Response> {
        let location = request.location.as_ref().map(|l| l.value.as_slice());
        let Some(ctx) = self
            .locations
            .iter()
            .position(|l| Some(l.as_slice()) == location)
        else {
            return NotFound.answer(request);
        };
        let data = request.data.as_ref()?;
        let named = || match data {
            rq::Data::Hash(h) => hash32(h),
            rq::Data::Number(n) => number(n).and_then(|n| self.hash_at(ctx, n)),
        };
        match request.request.as_ref()? {
            rq::Request::BlockHash(_) => match data {
                rq::Data::Number(n) => Some(rs::Response::BlockHash(ProtoHash {
                    value: number(n)
                        .and_then(|n| self.hash_at(ctx, n))
                        .map(|h| h.to_vec())
                        .unwrap_or_default(),
                })),
                rq::Data::Hash(_) => None,
            },
            rq::Request::WorkObjectBlock(_) => Some(rs::Response::WorkObjectBlockView(
                ProtoWorkObjectBlockView {
                    work_object: named().and_then(|h| self.object(ctx, &h)),
                },
            )),
            rq::Request::WorkObjectHeader(_) => Some(rs::Response::WorkObjectHeaderView(
                ProtoWorkObjectHeaderView {
                    work_object: named().and_then(|h| self.object(ctx, &h)).map(header_view),
                },
            )),
            rq::Request::WorkObjectBlocks(_) => Some(rs::Response::WorkObjectBlocksView(
                ProtoWorkObjectBlocksView {
                    work_objects: named().and_then(|h| self.from(ctx, &h)).unwrap_or_default(),
                },
            )),
            _ => NotFound.answer(request),
        }
    }
}
