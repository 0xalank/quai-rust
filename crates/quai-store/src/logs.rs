//! Bounded canonical log index. Preserve Go log objects and single-block order.
use super::{field, hash, quantity, Block, Receipt, Snapshot};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadMiss {
    Unsupported,
    Coverage,
    Order,
    Budget,
}
impl ReadMiss {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Coverage => "coverage",
            Self::Order => "order",
            Self::Budget => "budget",
        }
    }
}

fn hex(value: &str, bytes: usize) -> bool {
    value.len() == bytes * 2 + 2
        && value.starts_with("0x")
        && value.as_bytes()[2..].iter().all(u8::is_ascii_hexdigit)
}

pub(super) fn receipt_logs(receipt: &Value) -> Result<Vec<Value>, String> {
    let values = receipt
        .get("logs")
        .and_then(Value::as_array)
        .ok_or("missing receipt logs")?;
    let mut previous = None;
    for log in values {
        let index = quantity(field(log, "logIndex")?).ok_or("invalid log index")?;
        if log["blockHash"] != receipt["blockHash"]
            || log["blockNumber"] != receipt["blockNumber"]
            || log["transactionHash"] != receipt["transactionHash"]
            || log["transactionIndex"] != receipt["transactionIndex"]
            || log.get("removed").and_then(Value::as_bool) != Some(false)
            || !hex(field(log, "address")?, 20)
            || previous.is_some_and(|p| p >= index)
        {
            return Err("log inclusion metadata mismatch".into());
        }
        let data = field(log, "data")?;
        if !data.starts_with("0x")
            || data.len() % 2 != 0
            || !data.as_bytes()[2..].iter().all(u8::is_ascii_hexdigit)
        {
            return Err("invalid log data".into());
        }
        let topics = log
            .get("topics")
            .and_then(Value::as_array)
            .ok_or("missing log topics")?;
        if topics.len() > 4
            || topics
                .iter()
                .any(|v| !v.as_str().is_some_and(|s| hex(s, 32)))
        {
            return Err("invalid log topics".into());
        }
        previous = Some(index);
    }
    Ok(values.clone())
}

struct Entry {
    value: Value,
    number: u64,
}
#[derive(Default)]
pub(super) struct Index {
    entries: Vec<Entry>,
    addresses: HashMap<String, Vec<usize>>,
    topics: HashMap<(usize, String), Vec<usize>>,
}

impl Index {
    pub(super) fn build(
        canonical: &BTreeMap<u64, String>,
        blocks: &HashMap<String, Block>,
        receipts: &HashMap<(String, String), Receipt>,
    ) -> Result<Self, String> {
        let mut index = Self::default();
        for (number, key) in canonical {
            let mut expected = 0u64;
            for tx in &blocks[key].transactions {
                if let Some(receipt) = receipts.get(&(tx.clone(), key.clone())) {
                    for value in &receipt.logs {
                        if quantity(field(value, "logIndex")?) != Some(expected) {
                            return Err("canonical logs must have contiguous block indexes".into());
                        }
                        expected += 1;
                        if index.entries.len() >= 200_000 {
                            return Err("log index record limit".into());
                        }
                        let position = index.entries.len();
                        index
                            .addresses
                            .entry(field(value, "address")?.to_ascii_lowercase())
                            .or_default()
                            .push(position);
                        for (slot, topic) in value["topics"].as_array().unwrap().iter().enumerate()
                        {
                            index
                                .topics
                                .entry((slot, topic.as_str().unwrap().to_ascii_lowercase()))
                                .or_default()
                                .push(position);
                        }
                        index.entries.push(Entry {
                            value: value.clone(),
                            number: *number,
                        });
                    }
                }
            }
        }
        Ok(index)
    }
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}

fn choices(value: &Value, bytes: usize) -> Result<Vec<String>, ReadMiss> {
    let values: Vec<&Value> = match value {
        Value::String(_) => vec![value],
        Value::Array(v) if v.len() <= 256 => v.iter().collect(),
        _ => return Err(ReadMiss::Unsupported),
    };
    values
        .into_iter()
        .map(|v| {
            v.as_str()
                .filter(|s| hex(s, bytes))
                .map(str::to_ascii_lowercase)
                .ok_or(ReadMiss::Unsupported)
        })
        .collect()
}

impl Snapshot {
    /// Positive canonical receipts only. Unknown receipts always need Go in live mode.
    pub fn indexed_read(
        &self,
        method: &str,
        params: &Value,
        max_bytes: usize,
    ) -> Result<Vec<u8>, ReadMiss> {
        let args = params.as_array().ok_or(ReadMiss::Unsupported)?;
        if method == "quai_getTransactionReceipt" && args.len() == 1 {
            let tx = args[0]
                .as_str()
                .filter(|s| hash(s))
                .ok_or(ReadMiss::Unsupported)?;
            let block = self.transaction_blocks.get(tx).ok_or(ReadMiss::Coverage)?;
            let receipt = self
                .receipts
                .get(&(tx.to_owned(), block.clone()))
                .ok_or(ReadMiss::Coverage)?;
            return if receipt.encoded.len() <= max_bytes {
                Ok(receipt.encoded.clone())
            } else {
                Err(ReadMiss::Budget)
            };
        }
        if method != "quai_getLogs" || args.len() != 1 {
            return Err(ReadMiss::Unsupported);
        }
        self.indexed_logs(&args[0], max_bytes)
    }

    fn indexed_logs(&self, filter: &Value, max_bytes: usize) -> Result<Vec<u8>, ReadMiss> {
        if !self.complete {
            return Err(ReadMiss::Coverage);
        }
        let object = filter.as_object().ok_or(ReadMiss::Unsupported)?;
        if object.keys().any(|k| {
            ![
                "blockHash",
                "fromBlock",
                "toBlock",
                "address",
                "topics",
                "nodeLocation",
            ]
            .contains(&k.as_str())
        }) {
            return Err(ReadMiss::Unsupported);
        }
        let head = *self.head().0;
        let bounds = |v: Option<&Value>| -> Result<u64, ReadMiss> {
            match v {
                None | Some(Value::Null) => Ok(head),
                Some(Value::String(s)) if s == "latest" => Ok(head),
                Some(Value::String(s)) if s == "earliest" => Ok(0),
                Some(Value::String(s)) => quantity(s)
                    .filter(|n| *n <= i64::MAX as u64)
                    .ok_or(ReadMiss::Unsupported),
                _ => Err(ReadMiss::Unsupported),
            }
        };
        let (from, to) = match object.get("blockHash").filter(|v| !v.is_null()) {
            Some(value) => {
                if ["fromBlock", "toBlock"]
                    .iter()
                    .any(|k| object.get(*k).is_some_and(|v| !v.is_null()))
                {
                    return Err(ReadMiss::Unsupported);
                }
                let key = value
                    .as_str()
                    .filter(|s| hex(s, 32))
                    .ok_or(ReadMiss::Unsupported)?
                    .to_ascii_lowercase();
                let block = self.blocks.get(&key).ok_or(ReadMiss::Coverage)?;
                if self.canonical.get(&block.number) != Some(&key) {
                    return Err(ReadMiss::Coverage);
                }
                (block.number, block.number)
            }
            None => (
                bounds(object.get("fromBlock"))?,
                bounds(object.get("toBlock"))?,
            ),
        };
        if from > to
            || to - from > 10_000
            || from < *self.canonical.first_key_value().unwrap().0
            || to > head
        {
            return Err(ReadMiss::Coverage);
        }
        let addresses = match object.get("address") {
            None | Some(Value::Null) => Vec::new(),
            Some(v) => choices(v, 20)?,
        };
        let mut topics = Vec::new();
        match object.get("topics") {
            None | Some(Value::Null) => (),
            Some(Value::Array(v)) if v.len() <= 4 => {
                for rule in v {
                    if rule.is_null() {
                        topics.push(Vec::new());
                        continue;
                    }
                    // Go stops at the first null, including ignoring later entries.
                    if let Some(values) = rule.as_array() {
                        let mut parsed = Vec::new();
                        if values.len() > 256 {
                            return Err(ReadMiss::Unsupported);
                        }
                        for value in values {
                            if value.is_null() {
                                parsed.clear();
                                break;
                            }
                            parsed.push(
                                value
                                    .as_str()
                                    .filter(|v| hex(v, 32))
                                    .ok_or(ReadMiss::Unsupported)?
                                    .to_ascii_lowercase(),
                            );
                        }
                        topics.push(parsed);
                    } else {
                        topics.push(choices(rule, 32)?);
                    }
                }
            }
            _ => return Err(ReadMiss::Unsupported),
        }
        let union = |lists: Vec<&Vec<usize>>| -> Vec<usize> {
            lists
                .into_iter()
                .flatten()
                .copied()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        };
        let mut candidates = None;
        if !addresses.is_empty() {
            candidates = Some(union(
                addresses
                    .iter()
                    .filter_map(|a| self.log_index.addresses.get(a))
                    .collect(),
            ));
        }
        for (slot, values) in topics.iter().enumerate().filter(|(_, v)| !v.is_empty()) {
            let found = union(
                values
                    .iter()
                    .filter_map(|v| self.log_index.topics.get(&(slot, v.clone())))
                    .collect(),
            );
            if candidates
                .as_ref()
                .is_none_or(|old: &Vec<usize>| found.len() < old.len())
            {
                candidates = Some(found);
            }
        }
        let candidates = candidates.unwrap_or_else(|| (0..self.log_index.entries.len()).collect());
        if candidates.len() > 50_000 {
            return Err(ReadMiss::Budget);
        }
        let mut logs = Vec::new();
        let mut contributing_block = None;
        let mut bytes = 2usize;
        for position in candidates {
            let entry = &self.log_index.entries[position];
            if entry.number < from || entry.number > to {
                continue;
            }
            let value = &entry.value;
            if !addresses.is_empty()
                && !addresses.contains(&value["address"].as_str().unwrap().to_ascii_lowercase())
            {
                continue;
            }
            let actual = value["topics"].as_array().unwrap();
            if topics.len() > actual.len()
                || topics.iter().enumerate().any(|(slot, choices)| {
                    !choices.is_empty()
                        && !choices.contains(&actual[slot].as_str().unwrap().to_ascii_lowercase())
                })
            {
                continue;
            }
            // Go range workers append blocks nondeterministically. Preserve exact
            // observable ordering by forwarding multi-block nonempty results.
            if contributing_block.is_some_and(|n| n != entry.number) {
                return Err(ReadMiss::Order);
            }
            contributing_block = Some(entry.number);
            bytes += serde_json::to_vec(value)
                .map_err(|_| ReadMiss::Unsupported)?
                .len()
                + 1;
            if bytes > max_bytes || logs.len() >= 10_000 {
                return Err(ReadMiss::Budget);
            }
            logs.push(value);
        }
        serde_json::to_vec(&logs).map_err(|_| ReadMiss::Unsupported)
    }
}
