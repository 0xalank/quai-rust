//! Deterministic fuzz-style tests for execution decoders that read local
//! files or caller data: state images, early checkpoints, lockup records,
//! MuHash encodings and EVM precompiles. Inputs are random bytes plus bit
//! flips, truncation, extension, length-field edits and splices of valid
//! encodings.
use crate::{
    checkpoint,
    early_replay::EarlyReplay,
    evm::precompile,
    lockup::Lockup,
    muhash::MuHash,
    state_image::{self, hash, Reference},
};

#[path = "../../quai-primitives/tests/fuzz_support/mod.rs"]
mod fuzz;

const ITER: usize = 40_000;
fn budget(len: usize) -> usize {
    (2 << 20) + len.saturating_mul(128)
}

#[test]
fn fuzz_lockup_and_muhash() {
    let (_, world, _) = state_image::tests::sample();
    let seeds: Vec<Vec<u8>> = world
        .lockups()
        .unwrap()
        .iter()
        .map(|(_, v)| v.encode())
        .collect();
    assert!(!seeds.is_empty());
    let s = fuzz::drive("Lockup::decode", 51, &seeds, ITER, budget, |b| {
        let Ok(l) = Lockup::decode(b) else {
            return false;
        };
        // Decode accepts a 58-byte record with a zero delegate, which encode
        // writes as 38 bytes. Callers that need canonical bytes (state images)
        // compare encode(decode(x)) to x. The fixed point must hold.
        let enc = l.encode();
        assert_eq!(Lockup::decode(&enc).expect("own encoding"), l);
        assert_eq!(Lockup::decode(&enc).unwrap().encode(), enc);
        true
    });
    assert!(s.accepted > 100);
    let mut m = MuHash::default();
    m.add(b"a");
    m.add(b"b");
    let seeds = vec![m.serialize().to_vec(), vec![0; 384], vec![0xff; 384]];
    let s = fuzz::drive("MuHash::from_bytes", 52, &seeds, ITER, budget, |b| {
        let Ok(m) = MuHash::from_bytes(b) else {
            return false;
        };
        let _ = m.hash();
        assert_eq!(m.serialize().as_slice(), b);
        true
    });
    assert!(s.accepted > 100);
}

#[test]
fn fuzz_state_image_decode() {
    let (anchor, world, utxos) = state_image::tests::sample();
    let (image, reference) = state_image::encode(anchor, &world, &utxos).unwrap();
    let seeds = vec![image.clone()];
    // The digest is part of the trusted reference. Recompute it for each
    // mutation so the structural parser, not the digest check, sees the bytes.
    let s = fuzz::drive("state_image::decode", 53, &seeds, 20_000, budget, |b| {
        if b.len() < 32 {
            return state_image::decode(b, &reference).is_ok();
        }
        let mut input = b.to_vec();
        let end = input.len() - 32;
        let digest = hash(&input[..end]);
        input[end..].copy_from_slice(&digest);
        let expected = Reference {
            digest,
            anchor: reference.anchor.clone(),
            roots: reference.roots.clone(),
        };
        match state_image::decode(&input, &expected) {
            Ok(r) => {
                // Accepted bytes re-encode to the same image.
                let again = state_image::encode(expected.anchor.clone(), &r.world, &r.utxos)
                    .expect("restored state encodes");
                assert_eq!(again.0, input);
                true
            }
            Err(_) => false,
        }
    });
    assert!(s.accepted >= 1);
}

#[test]
fn fuzz_checkpoint_decode() {
    use crate::evm::{
        state::{Account, World},
        word::Word,
    };
    let mut world = World::default();
    for i in 1..4u8 {
        let mut address = [0; 20];
        address[19] = i;
        world
            .insert(
                address,
                Account {
                    nonce: u64::from(i),
                    balance: Word::from_u64(99 * u64::from(i)),
                    storage: std::collections::BTreeMap::from([([i; 32], [1; 32])]),
                    storage_size: Word::ONE,
                    code: vec![i; 5],
                    backing: None,
                },
            )
            .unwrap();
    }
    let genesis = [3u8; 32];
    let state = EarlyReplay::restore_local(world, genesis, [4; 32], 2, 1234).unwrap();
    let image = checkpoint::encode(&state).unwrap();
    assert!(checkpoint::decode(&image, genesis).is_ok());
    let seeds = vec![image];
    let s = fuzz::drive("checkpoint::decode", 54, &seeds, ITER, budget, |b| {
        let Ok(s) = checkpoint::decode(b, genesis) else {
            return false;
        };
        assert_eq!(checkpoint::encode(&s).unwrap(), b);
        true
    });
    assert!(s.accepted >= 1);
}

#[test]
fn fuzz_precompiles() {
    let mut rng = fuzz::Rng(55);
    let mut seeds: Vec<Vec<u8>> = (0..20).map(|_| rng.bytes(128)).collect();
    // ecrecover-shaped: hash, v = 27/28, r, s.
    for v in [27u8, 28] {
        let mut b = rng.bytes(128);
        b[32..63].fill(0);
        b[63] = v;
        seeds.push(b);
    }
    // modexp-shaped: three small lengths then data.
    let mut b = vec![0u8; 96];
    b[31] = 4;
    b[63] = 4;
    b[95] = 4;
    b.extend(rng.bytes(12));
    seeds.push(b);
    for id in 1..=9u8 {
        let s = fuzz::drive(&format!("precompile {id}"), 56 + u64::from(id), &seeds, 4_000, budget, |b| {
            // 2 million gas bounds the work a case can ask for.
            matches!(precompile::run(id, b, 2_000_000), Ok(Ok(_)))
        });
        let _ = s;
    }
}
