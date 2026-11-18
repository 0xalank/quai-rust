# Mainnet export format (QEX1)

QEX1 carries go-quai mainnet blocks, in import order, from the pinned Go node
database (`scripts/mainnet/go_node.sh`) to the Rust `replay` command. It is the
contract between the Go exporter and the Rust reader. Change it only with a new
magic (`QEX2`) and a reader for both versions.

Reference: go-quai v0.57.0 `33db82f2891b267ba898ca38bd2dd8bf75cbb072`,
environment `colosseum`, slice `[0 0]`.

## Directory

```
<dir>/manifest.json
<dir>/segments/<first>-<last>.qex     zone heights, 10 digits, zero padded
```

One segment holds the records for 100,000 consecutive zone heights (the last
segment can hold fewer). The exporter writes a segment to `<name>.pending` and
renames it when complete, so a reader never sees a partial segment. A resumed
export rewrites only the last, incomplete segment.

## Segment file

A segment starts with the 5-byte magic `QEX1\n`. Records follow until end of
file. All integers are big-endian.

| Field | Size | Value |
|---|---|---|
| kind | u8 | `1` = block |
| context | u8 | `0` prime, `1` region, `2` zone |
| number | u64 | block number in that context |
| hash | 32 bytes | `wo.Hash()` as go-quai computes it |
| block_len | u32 | length of `block` |
| block | bytes | `proto.Marshal(wo.ProtoEncode(types.BlockObject))` for the work object from `rawdb.ReadWorkObject(db, number, hash, types.BlockObject)` in that context's database |
| receipts_len | u32 | length of `receipts`; `0` for prime and region records, and when the manifest says `"receipts": false` |
| receipts | bytes | the bytes go-quai stores for this block's receipts in the zone database (storage protobuf form) |

## Record order

Records follow zone height, from 0 (genesis) upward, on the canonical zone
chain. For each zone height:

1. If the zone block is also a prime block, a prime record comes first.
2. If it is also a region block, a region record comes next.
3. The zone record comes last.

This is the order in which go-quai appends a coincident block (prime, then
region, then zone). Genesis has all three records. If a prime or region block
exists that is not coincident with a zone `[0 0]` block (more than one active
zone), the exporter must stop with an error until this format covers it.

## manifest.json

```json
{
  "format": "QEX1",
  "reference_revision": "33db82f2891b267ba898ca38bd2dd8bf75cbb072",
  "environment": "colosseum",
  "slice": [0, 0],
  "genesis_hash": "0x…",
  "receipts": true,
  "last_zone": 199999,
  "segments": [
    {"file": "segments/0000000000-0000099999.qex", "first_zone": 0, "last_zone": 99999, "records": 100123, "sha256": "…"}
  ],
  "exported_utc": "2026-11-17T00:00:00Z"
}
```

The reader checks `format`, `reference_revision`, the segment hashes and that
segment ranges are contiguous from 0.
