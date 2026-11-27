#![no_main]
use libfuzzer_sys::fuzz_target;
use quai_node::trie_sync::{extract_mpt_children, NodeKind};

fuzz_target!(|data: &[u8]| {
    let _ = extract_mpt_children(data, NodeKind::AccountTrie);
    let _ = extract_mpt_children(data, NodeKind::StorageTrie);
    let _ = extract_mpt_children(data, NodeKind::EtxTrie);
});
