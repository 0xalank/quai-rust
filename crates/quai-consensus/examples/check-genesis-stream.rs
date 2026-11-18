//! Native canonical import from an explicit offline genesis-linked Go stream.
//! There is no checkpoint or state-image import route in this executable.
#[allow(dead_code)]
#[path = "check-canonical.rs"]
mod canonical;
use quai_consensus::{history::Record, private_canonical::PrivateCanonical};
use quai_primitives::{decode_hex, encode_hex};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead},
    path::Path,
};
fn line(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut out = vec![];
    loop {
        let part = reader.fill_buf()?;
        if part.is_empty() {
            if out.is_empty() {
                return Ok(None);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated oracle record",
            ));
        }
        let n = part
            .iter()
            .position(|b| *b == b'\n')
            .map_or(part.len(), |n| n + 1);
        if out.len().checked_add(n).is_none_or(|n| n > 20 << 20) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "oracle record byte bound",
            ));
        }
        out.extend_from_slice(&part[..n]);
        reader.consume(n);
        if out.last() == Some(&b'\n') {
            return Ok(Some(out));
        }
    }
}
fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    assert_eq!(args.len(), 2, "expected datadir");
    let path = Path::new(&args[1]);
    let mut input = io::BufReader::new(io::stdin().lock());
    let initial: Value = serde_json::from_slice(&line(&mut input).unwrap().unwrap()).unwrap();
    assert_eq!(initial["kind"], "genesis");
    assert_eq!(
        initial["reference_revision"],
        quai_primitives::REFERENCE_REVISION
    );
    assert_eq!(initial["checkpoint_imported"], false);
    assert_eq!(initial["initial_allocations"], false);
    let total = initial["blocks"].as_u64().unwrap();
    assert!((1..=300000).contains(&total));
    let now = initial["now"].as_u64().unwrap();
    let mut chain = PrivateCanonical::open_execution_v2(path, now).unwrap();
    let start = chain.view().unwrap().height();
    assert!(start <= total);
    let genesis =
        Record::decode(&decode_hex(initial["work_object"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(
        chain.view().unwrap().hash_at(0).unwrap(),
        Some(genesis.id())
    );
    assert_eq!(initial["hash"], encode_hex(&genesis.id()));
    let mut n = 0u64;
    let mut checks = 6usize;
    let mut reopened = 0;
    let mut signed = 0;
    let mut contexts = 0;
    let mut done = false;
    while let Some(raw) = line(&mut input).unwrap() {
        let row: Value = serde_json::from_slice(&raw).unwrap();
        assert!(!done, "record after completion");
        match row["kind"].as_str().unwrap() {
            "block" => {
                n += 1;
                assert_eq!(row["height"], n);
                let bundle = canonical::bundle(&row);
                let block = Record::decode(&bundle.zone).unwrap();
                assert_eq!(block.number(2).unwrap(), n);
                contexts += bundle.contexts.len() + 1;
                if n <= start {
                    assert_eq!(chain.view().unwrap().hash_at(n).unwrap(), Some(block.id()));
                    checks += 1;
                } else {
                    let imported = chain
                        .import(bundle, now)
                        .unwrap_or_else(|e| panic!("native canonical height {n}: {e}"));
                    assert!(!imported.duplicate && !imported.global_consensus_qualified);
                    assert!(imported.selected);
                    checks += 2;
                }
                if n >= start {
                    let view = chain.view().unwrap();
                    assert_eq!(view.height(), n);
                    assert_eq!(view.block(), block.id());
                    assert_eq!(encode_hex(&view.roots().evm), row["evm_root"]);
                    assert_eq!(encode_hex(&view.roots().etx), row["etx_root"]);
                    assert_eq!(encode_hex(&view.roots().utxo), row["utxo_root"]);
                    assert_eq!(
                        view.roots().utxo_count,
                        row["utxo_set_size"].as_u64().unwrap()
                    );
                    assert_eq!(
                        view.roots().accounts,
                        row["quai_state_size"].as_u64().unwrap()
                    );
                    assert_eq!(chain.admitted_count(), n + 1);
                    assert_eq!(chain.durable_checkpoint().unwrap(), view.checkpoint());
                    assert!(!view.global_consensus_qualified());
                    checks += 10;
                    if n % 1024 == 0 {
                        let id = view.block();
                        let roots = view.roots().clone();
                        drop(view);
                        drop(chain);
                        chain = PrivateCanonical::open_execution_v2(path, now).unwrap();
                        let recovered = chain.view().unwrap();
                        assert_eq!(recovered.block(), id);
                        assert_eq!(recovered.roots(), &roots);
                        reopened += 1;
                        checks += 2;
                    }
                }
                signed = row["signed_quai_transactions"].as_u64().unwrap();
                assert_eq!(signed, n.saturating_sub(259200));
                checks += 1;
                if n % 1000 == 0 {
                    println!(
                        "{}",
                        json!({"kind":"progress","height":n,"resumed_from_height":start,"signed_quai_transactions":signed,"checks":checks})
                    );
                }
            }
            "complete" => {
                assert_eq!(n, total);
                assert_eq!(row["blocks"], total);
                assert_eq!(row["validated_from_genesis"], true);
                assert_eq!(row["checkpoint_imported"], false);
                assert_eq!(row["head"], encode_hex(&chain.view().unwrap().block()));
                done = true;
                checks += 5;
            }
            other => panic!("unknown oracle record {other}"),
        }
    }
    assert!(done);
    let view = chain.view().unwrap();
    println!(
        "{}",
        json!({"kind":"result","passed":true,"checks":checks,"blocks":n,"native_imports_this_run":n-start,"resumed_from_height":start,"head":encode_hex(&view.block()),"signed_quai_transactions":signed,"context_views":contexts,"writer_reopens":reopened,"profile":chain.profile_name(),"genesis_to_tip_local_receipts":true,"checkpoint_imported":false,"full_consensus":false,"canonical_public_sync":false,"runtime_go_dependency":false})
    );
}
