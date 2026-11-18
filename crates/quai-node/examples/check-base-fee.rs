//! `quai_gasPrice` on real blocks. go-quai's header validation requires the
//! base fee of every zone block to equal `HeaderChain.CalcBaseFee(parent)`
//! (core/headerchain_validation.go), so for each block N below the view's
//! head, `ChainView::calc_base_fee(N)` must equal the base fee of block
//! N + 1 as go-quai exported it. Prints one JSON line.
//!
//!     cargo run --release --example check-base-fee -- --export DIR --datadir DIR
use quai_node::log::{Level, Log};
use quai_node::rpc_view::ExportView;
use quai_rpc::view::ChainView;
use std::path::PathBuf;

fn main() {
    let mut export = None;
    let mut datadir = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--export" => export = args.next().map(PathBuf::from),
            "--datadir" => datadir = args.next().map(PathBuf::from),
            _ => panic!("usage: check-base-fee --export DIR --datadir DIR"),
        }
    }
    let (Some(export), Some(datadir)) = (export, datadir) else {
        panic!("usage: check-base-fee --export DIR --datadir DIR");
    };
    let view = ExportView::open(&export, &datadir, &Log::new(Level::Warn)).unwrap();
    let head = view.head();
    let (mut equal, mut up, mut down, mut flat) = (0u64, 0u64, 0u64, 0u64);
    let mut mismatches = vec![];
    let mut previous: Option<[u8; 32]> = None;
    let mut distinct = std::collections::BTreeSet::new();
    for n in 0..head {
        let block = view.block_by_number(n).unwrap().unwrap();
        let child = view.block_by_number(n + 1).unwrap().unwrap();
        let fee = view.calc_base_fee(&block).unwrap();
        let mut want = [0u8; 32];
        let raw = child
            .object
            .header()
            .wire()
            .base_fee
            .clone()
            .unwrap_or_default();
        want[32 - raw.len()..].copy_from_slice(&raw);
        if fee == want {
            equal += 1;
        } else if mismatches.len() < 10 {
            mismatches.push(n);
        }
        distinct.insert(fee);
        if let Some(p) = previous {
            match fee.cmp(&p) {
                std::cmp::Ordering::Greater => up += 1,
                std::cmp::Ordering::Less => down += 1,
                std::cmp::Ordering::Equal => flat += 1,
            }
        }
        previous = Some(fee);
    }
    println!(
        "{}",
        serde_json::json!({
            "head": head,
            "blocks": head,
            "equal_to_child_base_fee": equal,
            "mismatches": mismatches,
            "steps": {"up": up, "down": down, "flat": flat},
            "distinct_fees": distinct.len(),
        })
    );
    assert!(mismatches.is_empty());
}
