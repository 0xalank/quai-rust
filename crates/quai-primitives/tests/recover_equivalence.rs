//! Old (k256) against new (libsecp256k1) Quai ECDSA recovery.
//! Raw cases compare the recovery step. Transaction cases compare the whole
//! decode and sender path against the former acceptance rules.
use k256::ecdsa::{Signature, SigningKey};
use prost::Message;
use quai_primitives::{
    decode_hex,
    recover::{recover_k256, recover_pubkey},
    transaction::{Kind, Transaction},
    wire::tx as w,
};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn bytes32(&mut self) -> [u8; 32] {
        let mut o = [0u8; 32];
        for c in o.chunks_mut(8) {
            c.copy_from_slice(&self.next().to_le_bytes());
        }
        o
    }
}

const N: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe,
    0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36, 0x41, 0x41,
];
const HALF_N: [u8; 32] = [
    0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0x5d, 0x57, 0x6e, 0x73, 0x57, 0xa4, 0x50, 0x1d, 0xdf, 0xe9, 0x2f, 0x46, 0x68, 0x1b, 0x20, 0xa0,
];

fn add_small(v: &[u8; 32], d: i8) -> [u8; 32] {
    let mut o = *v;
    if d >= 0 {
        let mut carry = d as u16;
        for i in (0..32).rev() {
            let t = o[i] as u16 + carry;
            o[i] = t as u8;
            carry = t >> 8;
        }
    } else {
        let mut borrow = (-(d as i16)) as u16;
        for i in (0..32).rev() {
            let t = o[i] as i16 - borrow as i16;
            o[i] = t as u8;
            borrow = u16::from(t < 0);
        }
    }
    o
}
fn sub_n(v: &[u8; 32]) -> [u8; 32] {
    // N - v for v < N
    let mut o = [0u8; 32];
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let t = N[i] as i16 - v[i] as i16 - borrow;
        o[i] = t as u8;
        borrow = i16::from(t < 0);
    }
    o
}
fn small(x: u8) -> [u8; 32] {
    let mut o = [0u8; 32];
    o[31] = x;
    o
}
fn edge_scalars() -> Vec<[u8; 32]> {
    let mut v = vec![[0u8; 32], [0xff; 32], [0x80; 32]];
    for d in -3i8..=3 {
        v.push(add_small(&N, d));
        v.push(add_small(&HALF_N, d));
        v.push(add_small(&[0u8; 32], d.max(0)));
        let mut p = [0xffu8; 32];
        p[28..].copy_from_slice(&[0xff, 0xff, 0xfc, 0x2f]);
        p[27] = 0xfe;
        v.push(add_small(&p, d)); // around the field prime
    }
    v.push(small(1));
    v.push(small(2));
    v
}

#[test]
fn raw_recovery_matches_k256() {
    let mut rng = Rng(0x5eed_0001);
    let (mut cases, mut some, mut none) = (0usize, 0usize, 0usize);
    let mut check = |h: &[u8; 32], r: &[u8; 32], s: &[u8; 32], bit: u8| {
        let a = recover_k256(h, r, s, bit);
        let b = recover_pubkey(h, r, s, bit);
        assert_eq!(a, b, "r={r:02x?} s={s:02x?} bit={bit}");
        cases += 1;
        if a.is_some() {
            some += 1
        } else {
            none += 1
        }
    };
    // Edge table: every pair, every recovery id (0..=3 and out of range).
    let edges = edge_scalars();
    let h = rng.bytes32();
    for r in &edges {
        for s in &edges {
            for bit in 0..=4u8 {
                check(&h, r, s, bit);
            }
        }
    }
    // Fully random scalars and hashes, every recovery id.
    for _ in 0..25_000 {
        let (h, r, s) = (rng.bytes32(), rng.bytes32(), rng.bytes32());
        for bit in 0..=3 {
            check(&h, &r, &s, bit);
        }
    }
    // Valid signatures: low-s, high-s, both ids, tampered hash.
    for _ in 0..5_000 {
        let key = loop {
            if let Ok(k) = SigningKey::from_slice(&rng.bytes32()) {
                break k;
            }
        };
        let h = rng.bytes32();
        let (sig, id): (Signature, _) = key.sign_prehash_recoverable(&h).unwrap();
        let (r, s) = {
            let b = sig.to_bytes();
            let (mut r, mut s) = ([0u8; 32], [0u8; 32]);
            r.copy_from_slice(&b[..32]);
            s.copy_from_slice(&b[32..]);
            (r, s)
        };
        let high = sub_n(&s);
        let mut h2 = h;
        h2[(rng.next() % 32) as usize] ^= 1 << (rng.next() % 8);
        for bit in 0..=3 {
            check(&h, &r, &s, bit);
            check(&h, &r, &high, bit);
            check(&h2, &r, &s, bit);
        }
        // The signer's own id must recover the signer's key.
        let want = key.verifying_key().to_encoded_point(false);
        let got = recover_pubkey(&h, &r, &s, id.to_byte() & 1);
        if id.to_byte() < 2 {
            assert_eq!(got.as_ref().map(|k| &k[..]), Some(&want.as_bytes()[1..]));
        }
    }
    eprintln!("raw cases {cases}, recovered {some}, rejected {none}");
    assert!(cases >= 100_000 && some > 10_000 && none > 10_000);
}

fn vectors() -> Vec<Vec<u8>> {
    let c: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../fixtures/protocol/transactions.json"
    ))
    .unwrap();
    c["transactions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["type"] == 0 && v["sender"].is_string())
        .map(|v| decode_hex(v["input"].as_str().unwrap()).unwrap())
        .collect()
}

/// The acceptance rules the code had before the backend change.
fn old_sender(tx: &Transaction) -> Result<[u8; 20], ()> {
    use sha3::{Digest, Keccak256};
    let Kind::Quai(v) = tx.kind() else {
        return Err(());
    };
    let fixed = |b: &[u8]| {
        let mut o = [0u8; 32];
        let n = b.len().min(32);
        o[32 - n..].copy_from_slice(&b[b.len() - n..]);
        o
    };
    let (r, s) = (fixed(&v.r), fixed(&v.s));
    let sig = Signature::from_scalars(r, s).map_err(|_| ())?;
    if v.v.last().copied().unwrap_or(0) > 1 || sig.normalize_s().is_some() || v.v.len() > 1 {
        return Err(());
    }
    let key = recover_k256(
        &tx.signing_hash(),
        &r,
        &s,
        v.v.last().copied().unwrap_or(0),
    )
    .ok_or(())?;
    let mut a = [0u8; 20];
    a.copy_from_slice(&Keccak256::digest(key)[12..]);
    Ok(a)
}

#[test]
fn go_vectors_recover_the_pinned_sender() {
    let c: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../fixtures/protocol/transactions.json"
    ))
    .unwrap();
    let mut n = 0;
    for v in c["transactions"].as_array().unwrap() {
        if v["type"] == 0 && v["sender"].is_string() {
            let tx = Transaction::decode(&decode_hex(v["input"].as_str().unwrap()).unwrap()).unwrap();
            let want = decode_hex(v["sender"].as_str().unwrap()).unwrap();
            assert_eq!(tx.sender().unwrap().to_vec(), want, "{}", v["id"]);
            assert_eq!(old_sender(&tx).unwrap().to_vec(), want);
            n += 1;
        }
    }
    assert_eq!(n, 82);
    // Go-rejected inputs still fail to decode.
    let rej: Vec<_> = c["rejected"].as_array().unwrap().iter().collect();
    assert_eq!(rej.len(), 40);
    for v in rej {
        assert!(Transaction::decode(&decode_hex(v["input"].as_str().unwrap()).unwrap()).is_err());
    }
}

#[test]
fn transaction_sender_path_matches_old_rules() {
    let base = vectors();
    let mut rng = Rng(0x5eed_0002);
    let edges = edge_scalars();
    let trim = |b: &[u8; 32]| -> Vec<u8> {
        let s = b.iter().position(|x| *x != 0).unwrap_or(32);
        b[s..].to_vec()
    };
    let (mut cases, mut ok, mut err) = (0usize, 0usize, 0usize);
    let mut digest = 0u64;
    for i in 0..30_000usize {
        let mut p = w::Transaction::decode(base[i % base.len()].as_slice()).unwrap();
        let r = if i % 3 == 0 { edges[(rng.next() as usize) % edges.len()] } else { rng.bytes32() };
        let s = match i % 4 {
            0 => edges[(rng.next() as usize) % edges.len()],
            1 => {
                let l = rng.bytes32();
                // forced low or high s derived from the same value
                let mut l = l;
                l[0] &= 0x7f;
                if i % 8 == 1 { l } else { sub_n(&l) }
            }
            _ => rng.bytes32(),
        };
        p.r = Some(trim(&r));
        p.s = Some(trim(&s));
        p.v = Some(match rng.next() % 6 {
            0 => vec![],
            1 => vec![0],
            2 => vec![1],
            3 => vec![2],
            4 => vec![0, 1],
            _ => vec![(rng.next() & 0xff) as u8],
        });
        p.nonce = Some(rng.next());
        let bytes = p.encode_to_vec();
        let old = Transaction::decode(&bytes).map_err(|_| ()).and_then(|t| old_sender(&t));
        let new = Transaction::decode(&bytes).map_err(|_| ()).and_then(|t| t.sender().map_err(|_| ()));
        assert_eq!(old, new, "case {i}");
        cases += 1;
        match new {
            Ok(a) => {
                ok += 1;
                digest = digest.wrapping_mul(1_000_003).wrapping_add(u64::from_le_bytes(a[..8].try_into().unwrap()));
            }
            Err(_) => err += 1,
        }
    }
    eprintln!("tx cases {cases}, ok {ok}, err {err}, digest {digest:016x}");
    assert!(ok > 1_000 && err > 1_000);
}
