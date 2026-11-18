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
export rewrites only the last, incomplete segment. That segment can get a new
`<last>` in its name; the exporter deletes the old file after the manifest no
longer lists it. A reader takes the file names from the manifest only.

A peer export can be pruned (see [Pruned segments](#pruned-segments)): the
manifest still lists a pruned segment, but its file is gone.

## Segment file

A segment starts with the 5-byte magic `QEX1\n`. Records follow until end of
file. All integers are big-endian.

| Field | Size | Value |
|---|---|---|
| kind | u8 | `1` = block |
| context | u8 | `0` prime, `1` region, `2` zone |
| number | u64 | block number in that context: `wo.NumberU64(context)` |
| hash | 32 bytes | `wo.Hash()` as go-quai computes it; one value for the prime, region and zone records of a block |
| block_len | u32 | length of `block` |
| block | bytes | `proto.Marshal(wo.ProtoEncode(types.BlockObject))` for the work object from `rawdb.ReadWorkObject(db, number, hash, types.BlockObject)` in that context's database |
| receipts_len | u32 | length of `receipts`; `0` for prime and region records, and when the manifest says `"receipts": false` |
| receipts | bytes | the bytes go-quai stores for this block's receipts in the zone database (storage protobuf form, see [Receipts](#receipts)) |

The prime and region work objects of a block have the same header as its zone
work object, but a different body: go-quai builds the body for each context.
A dominant body has no transactions, outbound ETXs or uncles; it has the
manifest of the next context down, and the prime body also has the interlink
hashes. After genesis, the `block` bytes of the three records of one block
are therefore different.

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

## Sources

Reference go-quai v0.57.0 stores each context in its own database:
`<data-dir>/prime`, `<data-dir>/region-0` and `<data-dir>/zone-0-0`, each with
the key-value store in `go-quai/chaindata` and the ancient store in
`go-quai/chaindata/ancient`. The exporter opens them read-only with go-quai's
`rawdb.Open`, with the node locations `[]`, `[0]` and `[0 0]`, on an APFS clone
of the node database (`scripts/mainnet/export.sh`). v0.57.0 never starts its
freezer, so the ancient store is empty, but reads go through it first as in
go-quai.

- The export ends at the zone head, `rawdb.ReadHeadBlockHash` of the zone
  database: the last block whose state go-quai has processed. (`-max-zone`
  can end it earlier.)
- The zone block at height `z` is `rawdb.ReadCanonicalHash(zone, z)`.
- A zone block with hash `h` is also a prime (region) block when
  `rawdb.ReadCanonicalHash(prime, wo.NumberU64(0)) == h`
  (`rawdb.ReadCanonicalHash(region, wo.NumberU64(1)) == h`). The record
  `number` is that dominant number.

## Receipts

The `receipts` field of a zone record holds, unchanged, the value of the zone
database key

```
"r" (0x72) || zone number (u64, big-endian) || block hash (32 bytes)
```

(`blockReceiptsKey` in `core/rawdb/schema.go`). `rawdb.WriteReceipts` writes it
with `Receipts.Bytes()`: `proto.Marshal` of `ProtoReceiptsForStorage`
(`core/types/proto_block.proto`):

```proto
message ProtoReceiptsForStorage { repeated ProtoReceiptForStorage receipts = 1; }
message ProtoReceiptForStorage {
  bytes post_state_or_status = 1;
  uint64 cumulative_gas_used = 2;
  ProtoLogsForStorage logs = 3;
  common.ProtoHash tx_hash = 4;
  common.ProtoAddress contract_address = 5;
  uint64 gas_used = 6;
  ProtoTransactions outbound_etxs = 7;
}
```

go-quai writes this value only after `ValidateState` has accepted the block,
and genesis gets an empty list. A zone record with `receipts_len` 0 in an export
with `"receipts": true` has an empty stored list: genesis, and every block
without transactions. The exporter stops if a zone block has no entry. The
storage form has no receipt type and no bloom. The state processor gives each
receipt the type of its transaction (the block transaction with the same
`tx_hash`), and `ReceiptForStorage.ProtoDecode` computes the bloom from the
logs. With these, `types.DeriveSha` of the receipts equals the header receipt
root.

## Exporter checks

The exporter checks every record before it writes it, and
`quai-mainnet-export -verify` checks a finished export again from its files.
An export that passes both has these properties:

- The genesis hash in the three contexts is the mainnet genesis
  `0xac81c28f1a72591b87b5f16c9793cdc0e87c45c6d426d1a364c3b8f6386b5b8b`
  (`params.ProgpowColosseumGenesisHash`).
- The `block` bytes decode and encode back to the same bytes, and the hash
  recomputed from them is the record `hash`, which is the canonical hash in
  that context's database.
- The work object header hash binds the body header, and the body matches the
  header roots as `ValidateBody` checks them: transaction, outbound ETX and
  uncle roots for zone records; manifest root (and interlink root for prime)
  for dominant records. The zone receipts match the header receipt root.
- Each context is a hash-linked chain from genesis without gaps. Each zone
  block's prime and region parents are the last prime and region records, so
  every dominant block is coincident with a zone `[0 0]` block. After the last
  record, the next canonical prime and region blocks are zone `[0 0]` blocks
  above the export.
- A zone block that the prime or region database stores is canonical there.
  A database copy that breaks this (a clone of a running node at a bad moment)
  stops the export.
- The expansion number of every block is 0: one region with one zone.

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

`records` is the number of records in the segment; `last_zone` is the last
zone height of the export; `exported_utc` is the time of the last manifest
write. The exporter writes the manifest to `manifest.json.tmp` and renames it,
after each segment (and `peer-export prune` after it prunes).

The reader checks `format`, `reference_revision`, the segment hashes and that
segment ranges are contiguous from 0.

## Pruned segments

A long replay does not need the segments behind its checkpoints, and a full
mainnet export (about 65 GB) does not fit next to the replay on the reference
machine. `peer-export prune DIR --below ZONE` deletes the files of the
complete segments that end below `ZONE` and marks them in the manifest:

```json
{"file": "segments/0000000000-0000099999.qex", "first_zone": 0, "last_zone": 99999, "records": 174087, "sha256": "…", "pruned": true}
```

- A pruned entry keeps its range, record count and hash, so segment ranges
  stay contiguous from 0 and `last_zone` is unchanged. `"pruned"` is written
  only when it is `true`; an entry without it is kept.
- Only a prefix of the segments can be pruned, and never the last segment.
  `prune` also keeps the newest complete segment: a resumed export reads its
  chain tips from that segment's last zone height. A zone block names its
  prime and region parents, and the exporter checked those links when it
  wrote the block.
- The manifest is written before the files are deleted. An interrupted prune
  leaves files of pruned segments, which the next prune or export deletes;
  never a listed segment without its file. The `sources/` file of a pruned
  segment stays.
- A replay that resumes from a checkpoint at zone `h` reads only the segments
  from the one that holds `h`. The rolling replay
  (`scripts/mainnet/rolling_replay.sh`) passes the oldest checkpoint that the
  replay keeps as `ZONE`, so a fallback to that checkpoint still finds its
  segment.

Readers never open a pruned segment. The Rust reader
(`crates/quai-node/src/qex.rs`) fails with an error that names the segment
and the first kept zone height when a replay must read one. `peer-export`
resumes and extends a pruned export, and `peer-export verify` checks the kept
segments, with the chain rules from the first kept zone block on (its own
parent links stand for the pruned blocks). `peer-export compare` needs every
segment. Go exports (`"receipts": true`) are not pruned. A reader that does
not know the field fails on the missing file, so an older reader cannot
misread a pruned export; the segment format stays QEX1.
