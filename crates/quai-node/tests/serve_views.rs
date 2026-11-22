//! The block and header views this node answers to peers' WorkObjectBlock
//! and WorkObjectHeader requests, byte for byte against go-quai v0.57.0's
//! handleBlockRequest (fixtures/protocol/header-views.json,
//! scripts/gen_header_view_vectors.py): prime, region and zone work objects
//! of mainnet after KawPow and of the private hierarchy fixture.
//!
//! A node that synced from go-quai peers stores each block as go-quai sent
//! it, which is go-quai's own block view encoding: for the 36 mainnet
//! records that holds byte for byte. The private fixture's work objects
//! come from a test generator; go-quai's decode and encode adds empty
//! fields to them, so their stored form here is go-quai's block view.
use prost::Message;
use quai_primitives::schema::block::{ProtoWorkObjectBlockView, ProtoWorkObjectHeaderView};
use quai_primitives::{decode_hex, encode_hex};

#[test]
fn block_and_header_views_equal_go_quai() {
    let doc: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/header-views.json")).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    let mut by_context = [0usize; 3];
    let mut mainnet = 0;
    for c in cases {
        let name = c["name"].as_str().unwrap();
        assert!(
            c.get("error").is_none(),
            "{name}: go-quai could not decode it"
        );
        let go_block = decode_hex(c["block_view"].as_str().unwrap()).unwrap();
        let stored = ProtoWorkObjectBlockView::decode(go_block.as_slice())
            .unwrap()
            .work_object
            .unwrap();
        if name.starts_with("mainnet/") {
            mainnet += 1;
            assert_eq!(
                encode_hex(&stored.encode_to_vec()),
                c["block"].as_str().unwrap(),
                "{name}: mainnet bytes are go-quai's block view"
            );
        }
        // The block view: the stored work object as it is.
        let block = ProtoWorkObjectBlockView {
            work_object: Some(stored.clone()),
        };
        assert_eq!(block.encode_to_vec(), go_block, "{name}: block view");
        // The header view.
        let header = ProtoWorkObjectHeaderView {
            work_object: Some(quai_node::serve::header_view(stored)),
        };
        assert_eq!(
            encode_hex(&header.encode_to_vec()),
            c["header_view"].as_str().unwrap(),
            "{name}: header view"
        );
        by_context[c["context"].as_u64().unwrap() as usize] += 1;
    }
    assert!(by_context.iter().all(|n| *n >= 12), "{by_context:?}");
    assert_eq!(mainnet, 36);
}
