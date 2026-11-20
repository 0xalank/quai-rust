use quai_execution::{
    economic_state::magnitude,
    evm::{
        state::{Account, World},
        word::Word,
    },
    historical::{self, History},
    qi::{OutPoint, Utxo},
    settlement::Error,
    utxo::{self, UtxoState},
};
use quai_primitives::{decode_hex, encode_hex, transaction::Transaction, Hash};
use serde_json::{json, Value};
use std::collections::BTreeMap;
#[derive(Default)]
struct Branch {
    incoming: BTreeMap<u64, Vec<Transaction>>,
    created: BTreeMap<u64, Vec<(OutPoint, u8)>>,
}
impl History for Branch {
    fn incoming_at(&self, h: u64) -> Result<Vec<Transaction>, Error> {
        self.incoming
            .get(&h)
            .cloned()
            .ok_or(Error::Invalid("missing historical incoming"))
    }
    fn created_at(&self, h: u64) -> Result<Vec<(OutPoint, u8)>, Error> {
        Ok(self.created.get(&h).cloned().unwrap_or_default())
    }
}
fn compare(
    checks: &mut usize,
    failures: &mut Vec<Value>,
    row: &Value,
    id: Value,
    field: &str,
    actual: Value,
) {
    *checks += 1;
    if actual != row[field] {
        failures.push(json!({"id":id,"field":field,"actual":actual,"expected":row[field]}));
    }
}
pub fn verify(v: &Value) -> Value {
    assert_eq!(v["reference_revision"], quai_primitives::REFERENCE_REVISION);
    let mut checks = 0;
    let mut failures = vec![];
    for row in v["allocations"].as_array().unwrap() {
        let mut world = World::default();
        let h = row["height"].as_u64().unwrap();
        let p = row["prime"].as_u64().unwrap();
        world.begin_block();
        historical::allocate(&mut world, h, p, [0, 0]).unwrap();
        world.finish_block().unwrap();
        for (field, actual) in [
            ("root", json!(encode_hex(&world.root().unwrap()))),
            ("accounts", json!((world.account_count() as usize))),
        ] {
            compare(
                &mut checks,
                &mut failures,
                row,
                json!(format!("alloc-{h}-{p}")),
                field,
                actual,
            );
        }
    }
    for row in v["redemptions"].as_array().unwrap() {
        let mut world = World::default();
        let mut address = [0; 20];
        address[18] = 0x11;
        address[19] = 1;
        if row["exists"] == true {
            world
                .insert(
                    address,
                    Account {
                        nonce: 1,
                        balance: Word::from_u64(100),
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        compare(
            &mut checks,
            &mut failures,
            row,
            row["id"].clone(),
            "before_root",
            json!(encode_hex(&world.root().unwrap())),
        );
        let mut branch = Branch::default();
        for source in row["sources"].as_array().unwrap() {
            branch.incoming.insert(
                source["height"].as_u64().unwrap(),
                source["transactions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| {
                        Transaction::decode(&decode_hex(v.as_str().unwrap()).unwrap()).unwrap()
                    })
                    .collect(),
            );
        }
        let unlocks = historical::redeem(
            &mut world,
            &branch,
            row["height"].as_u64().unwrap(),
            Word::from_u64(1 << 20),
            [0, 0],
        )
        .unwrap();
        compare(
            &mut checks,
            &mut failures,
            row,
            row["id"].clone(),
            "root",
            json!(encode_hex(&world.root().unwrap())),
        );
        compare(&mut checks,&mut failures,row,row["id"].clone(),"unlocks",json!(unlocks.iter().map(|u|json!({"address":encode_hex(&u.address),"amount":encode_hex(&magnitude(u.amount))})).collect::<Vec<_>>()));
    }
    for row in v["trims"].as_array().unwrap() {
        let mut address = [0; 20];
        address[1] = 128;
        address[18] = 0x11;
        address[19] = 1;
        let mut hash: Hash = [0; 32];
        hash[31] = 1;
        let point = OutPoint { hash, index: 0 };
        let locked = OutPoint { hash, index: 1 };
        let value = Utxo {
            denomination: 0,
            address: address.into(),
            lock: vec![],
        };
        let mut base = UtxoState::default();
        base.insert(point, value.clone()).unwrap();
        base.insert(
            locked,
            Utxo {
                lock: vec![1],
                ..value.clone()
            },
        )
        .unwrap();
        let mut state = base.clone();
        let mut spent = vec![];
        if row["spent"] == true {
            spent.push(utxo::utxo_hash(point, &value));
            state.remove(&point).unwrap();
        }
        let mut branch = Branch::default();
        let mut keys = vec![(point, 0), (locked, 0)];
        if row["duplicate"] == true {
            keys.push((point, 0));
        }
        branch.created.insert(1, keys);
        let (trimmed, removed) = historical::trim(
            &mut state,
            &base,
            &branch,
            241921,
            row["prime"].as_u64().unwrap(),
            &spent,
        )
        .unwrap();
        for (field, actual) in [
            ("root", json!(encode_hex(&state.root()))),
            ("size", json!(state.consensus_size())),
            ("trimmed", json!(trimmed.len())),
            ("removed", json!(encode_hex(&magnitude(removed)))),
        ] {
            compare(
                &mut checks,
                &mut failures,
                row,
                row["id"].clone(),
                field,
                actual,
            );
        }
    }
    json!({"passed":failures.is_empty(),"checks":checks,"cases":60,"failures":failures,"full_consensus":false})
}
fn main() {
    let p = std::env::args().nth(1).expect("historical corpus");
    let v: Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
    let r = verify(&v);
    println!("{}", serde_json::to_string_pretty(&r).unwrap());
    if r["passed"] != true {
        std::process::exit(1);
    }
}
