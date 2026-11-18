// Command quai-mainnet-export writes the canonical go-quai mainnet chain
// (environment colosseum, slice [0 0]) in the QEX1 format of
// docs/mainnet-export-format.md, for the Rust `replay` command
// (ROADMAP.md, Phase 1).
//
// It is an explicit offline Go oracle. scripts/mainnet/export.sh compiles it
// into the pinned go-quai module with `go build -overlay` (together with
// rawdb-export.go) and runs it on an APFS clone of the node database. It opens
// the three context databases read-only through go-quai's own rawdb.Open, so
// the key-value store and the ancient store both work.
//
//	quai-mainnet-export -db DIR -out DIR [-max-zone N] [-receipts=true]
//	quai-mainnet-export -verify -out DIR
//
// The -db directory holds prime/, region-0/ and zone-0-0/: a copy of the
// node's --global.data-dir. Every record is checked before it is written, and
// -verify checks a finished export again from its files alone.
package main

import (
	"bufio"
	"bytes"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"hash"
	"io"
	"os"
	"path/filepath"
	"regexp"
	"syscall"
	"time"

	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/ethdb"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"github.com/dominant-strategies/go-quai/trie"
	"github.com/sirupsen/logrus"
	"google.golang.org/protobuf/proto"
)

// referenceRevision is set with -ldflags "-X main.referenceRevision=<commit>".
var referenceRevision string

const (
	pinnedRevision  = "33db82f2891b267ba898ca38bd2dd8bf75cbb072"
	formatName      = "QEX1"
	segmentMagic    = "QEX1\n"
	segmentHeights  = 100000
	recordKindBlock = 1
	environmentName = "colosseum"
	progressEvery   = 10000
	recordHeaderLen = 1 + 1 + 8 + 32 + 4 // kind, context, number, hash, block_len
)

const (
	primeCtx  = common.PRIME_CTX
	regionCtx = common.REGION_CTX
	zoneCtx   = common.ZONE_CTX
)

var (
	contextNames     = [3]string{"prime", "region", "zone"}
	contextDirs      = [3]string{"prime", "region-0", "zone-0-0"}
	contextLocations = [3]common.Location{{}, {0}, {0, 0}}
	contextCacheMB   = [3]int{256, 256, 1024}
	zoneLocation     = common.Location{0, 0}
	segmentName      = regexp.MustCompile(`^segments/[0-9]{10}-[0-9]{10}\.qex$`)
)

func main() {
	// go-quai logs to ./nodelogs and stdout by default. Keep stdout for the
	// JSON progress lines and send go-quai's own warnings to stderr.
	log.Global.SetOutput(os.Stderr)
	log.Global.SetLevel(logrus.WarnLevel)

	dbRoot := flag.String("db", "", "directory with prime/, region-0/ and zone-0-0/ (a copy of the node's --global.data-dir)")
	outDir := flag.String("out", "", "export directory (manifest.json and segments/)")
	maxZone := flag.Int64("max-zone", -1, "last zone height to export (default: the zone head)")
	withReceipts := flag.Bool("receipts", true, "export the stored receipts of zone blocks")
	minFreeGB := flag.Uint64("min-free-gb", 30, "stop when the output volume has less free space, in GiB")
	verify := flag.Bool("verify", false, "check an existing export from its files; no database")
	flag.Parse()

	if referenceRevision != pinnedRevision {
		fatalf("build with -ldflags \"-X main.referenceRevision=%s\" in the pinned go-quai checkout", pinnedRevision)
	}
	if *outDir == "" || flag.NArg() != 0 || (*dbRoot == "") != *verify {
		flag.Usage()
		os.Exit(2)
	}
	var err error
	if *verify {
		err = verifyExport(*outDir)
	} else {
		err = runExport(*dbRoot, *outDir, *maxZone, *withReceipts, *minFreeGB)
	}
	if err != nil {
		fatalf("%v", err)
	}
}

func fatalf(format string, args ...interface{}) {
	fmt.Fprintf(os.Stderr, "quai-mainnet-export: error: "+format+"\n", args...)
	os.Exit(1)
}

// emit prints one JSON line on stdout.
func emit(v interface{}) {
	line, err := json.Marshal(v)
	if err != nil {
		fatalf("encode output: %v", err)
	}
	fmt.Println(string(line))
}

// ---------------------------------------------------------------------------
// Records and the chain rules of an export.

type record struct {
	ctx      int
	number   uint64
	hash     common.Hash
	block    []byte
	receipts []byte
}

func (r *record) size() uint64 {
	return uint64(recordHeaderLen + len(r.block) + 4 + len(r.receipts))
}

func (r *record) describe() string {
	return fmt.Sprintf("%s block %d (%s)", contextNames[r.ctx], r.number, r.hash.Hex())
}

type tip struct {
	number uint64
	hash   common.Hash
	set    bool
}

// chainChecker applies the QEX1 rules to records in stream order. The
// exporter checks every record with it before writing, and -verify checks
// every record read back from disk, so both sides use the same rules:
//
//   - The block bytes decode, encode back to the same bytes, and the work
//     object hash recomputed from them is the record hash.
//   - The body matches the header roots, as go-quai's ValidateBody checks it,
//     and the stored receipts match the header receipt root.
//   - Each context is a hash-linked chain from the mainnet genesis block.
//   - Zone heights increase by one. At one zone height a prime record comes
//     first, a region record next and the zone record last, all of one block.
//   - Every prime and region block is coincident with a zone [0 0] block:
//     the dominant parents of each zone block are the last exported blocks of
//     those contexts. A dominant block from another zone breaks this rule.
//   - The expansion number is 0: one region with one zone is active.
type chainChecker struct {
	receipts     bool
	last         [3]tip
	nextZone     uint64
	pending      [2]*record
	checked      [3]uint64
	transactions uint64
	receiptCount uint64
	receiptBytes uint64
	expansion    map[uint8]uint64
}

func newChainChecker(receipts bool) *chainChecker {
	return &chainChecker{receipts: receipts, expansion: map[uint8]uint64{}}
}

func (c *chainChecker) check(r *record) error {
	if r.ctx < primeCtx || r.ctx > zoneCtx {
		return fmt.Errorf("unknown context %d", r.ctx)
	}
	wo, err := decodeBlock(r.block, r.ctx)
	if err != nil {
		return fmt.Errorf("%s: %v", r.describe(), err)
	}
	if got := wo.Hash(); got != r.hash {
		return fmt.Errorf("%s: the work object hash recomputed from the record is %s", r.describe(), got.Hex())
	}
	if got := wo.NumberU64(r.ctx); got != r.number {
		return fmt.Errorf("%s: the header number in this context is %d", r.describe(), got)
	}
	if err := checkBody(wo, r.ctx); err != nil {
		return fmt.Errorf("%s: %v", r.describe(), err)
	}
	if e := wo.ExpansionNumber(); e != 0 {
		regions, zones := common.GetHierarchySizeForExpansionNumber(e)
		return fmt.Errorf("%s: expansion number %d (%d regions x %d zones active); QEX1 covers one active zone only", r.describe(), e, regions, zones)
	}
	if r.number != 0 && !wo.Location().Equal(zoneLocation) {
		return fmt.Errorf("%s: location %v is not zone [0 0]", r.describe(), wo.Location())
	}
	if r.ctx != zoneCtx && len(r.receipts) != 0 {
		return fmt.Errorf("%s: a %s record has receipts", r.describe(), contextNames[r.ctx])
	}

	// Coincident blocks: prime, then region, then the zone record of the block.
	switch r.ctx {
	case primeCtx:
		if c.pending[primeCtx] != nil || c.pending[regionCtx] != nil {
			return fmt.Errorf("%s: a dominant record before it has no zone record", r.describe())
		}
		c.pending[primeCtx] = r
	case regionCtx:
		if c.pending[regionCtx] != nil {
			return fmt.Errorf("%s: two region records at one zone height", r.describe())
		}
		if p := c.pending[primeCtx]; p != nil && p.hash != r.hash {
			return fmt.Errorf("%s: follows the prime record of block %s", r.describe(), p.hash.Hex())
		}
		c.pending[regionCtx] = r
	case zoneCtx:
		if r.number != c.nextZone {
			return fmt.Errorf("%s: expected zone height %d", r.describe(), c.nextZone)
		}
		if c.pending[primeCtx] != nil && c.pending[regionCtx] == nil {
			return fmt.Errorf("%s: a prime block without a region record", r.describe())
		}
		for ctx := primeCtx; ctx <= regionCtx; ctx++ {
			if p := c.pending[ctx]; p != nil && p.hash != r.hash {
				return fmt.Errorf("%s: the %s record before it is of block %s", r.describe(), contextNames[ctx], p.hash.Hex())
			}
		}
	}

	// Hash links in the record's context.
	if r.number == 0 {
		if r.hash != params.ProgpowColosseumGenesisHash {
			return fmt.Errorf("%s: not the mainnet genesis %s", r.describe(), params.ProgpowColosseumGenesisHash.Hex())
		}
		if c.last[r.ctx].set {
			return fmt.Errorf("%s: a second genesis record", r.describe())
		}
		if r.ctx == zoneCtx && (c.pending[primeCtx] == nil || c.pending[regionCtx] == nil) {
			return fmt.Errorf("%s: genesis needs prime and region records", r.describe())
		}
	} else {
		prev := c.last[r.ctx]
		if !prev.set || prev.number+1 != r.number {
			return fmt.Errorf("%s: the previous %s record is %d", r.describe(), contextNames[r.ctx], prev.number)
		}
		if parent := wo.ParentHash(r.ctx); parent != prev.hash {
			return fmt.Errorf("%s: parent hash %s is not the previous %s record %s", r.describe(), parent.Hex(), contextNames[r.ctx], prev.hash.Hex())
		}
	}
	if r.ctx != zoneCtx {
		return nil
	}

	// A zone block that is not itself a prime (region) block points at the
	// last exported prime (region) block. If another zone had produced a
	// dominant block, the dominant parent would be that block instead.
	if r.number != 0 {
		if err := c.checkDominantParents(wo, r.describe()); err != nil {
			return err
		}
	}
	if c.receipts {
		count, err := checkReceipts(wo, r.receipts)
		if err != nil {
			return fmt.Errorf("%s: %v", r.describe(), err)
		}
		c.receiptCount += uint64(count)
		c.receiptBytes += uint64(len(r.receipts))
	} else if len(r.receipts) != 0 {
		return fmt.Errorf("%s: receipts in an export without receipts", r.describe())
	}
	c.transactions += uint64(len(wo.Transactions()))
	for ctx := primeCtx; ctx <= regionCtx; ctx++ {
		if p := c.pending[ctx]; p != nil {
			c.last[ctx] = tip{number: p.number, hash: p.hash, set: true}
			c.checked[ctx]++
			c.pending[ctx] = nil
		}
	}
	c.last[zoneCtx] = tip{number: r.number, hash: r.hash, set: true}
	c.checked[zoneCtx]++
	c.expansion[wo.ExpansionNumber()]++
	c.nextZone = r.number + 1
	return nil
}

// checkDominantParents checks the prime and region parents of a zone block
// against the last exported dominant blocks, for the contexts in which the
// block itself has no record at this zone height.
func (c *chainChecker) checkDominantParents(wo *types.WorkObject, what string) error {
	for ctx := primeCtx; ctx <= regionCtx; ctx++ {
		if c.pending[ctx] != nil {
			continue
		}
		prev := c.last[ctx]
		parent, number := wo.ParentHash(ctx), wo.NumberU64(ctx)
		if parent != prev.hash || number != prev.number+1 {
			return fmt.Errorf("%s: its %s parent is %s (%s height %d), but the last exported %s block is %s (%d): a %s block is not coincident with a zone [0 0] block, which QEX1 does not cover",
				what, contextNames[ctx], parent.Hex(), contextNames[ctx], number-1, contextNames[ctx], prev.hash.Hex(), prev.number, contextNames[ctx])
		}
	}
	return nil
}

// complete reports an error if the stream ended inside a zone height.
func (c *chainChecker) complete() error {
	if c.pending[primeCtx] != nil || c.pending[regionCtx] != nil {
		return errors.New("the records end with a dominant record that has no zone record")
	}
	return nil
}

func encodeBlock(wo *types.WorkObject) ([]byte, error) {
	protoWo, err := wo.ProtoEncode(types.BlockObject)
	if err != nil {
		return nil, err
	}
	return proto.Marshal(protoWo)
}

// decodeBlock decodes block bytes as go-quai decodes a work object of the
// context database, and checks that they encode back to the same bytes.
func decodeBlock(data []byte, ctx int) (*types.WorkObject, error) {
	protoWo := new(types.ProtoWorkObject)
	if err := proto.Unmarshal(data, protoWo); err != nil {
		return nil, fmt.Errorf("decode protobuf: %v", err)
	}
	wo := new(types.WorkObject)
	if err := wo.ProtoDecode(protoWo, contextLocations[ctx], types.BlockObject); err != nil {
		return nil, fmt.Errorf("decode work object: %v", err)
	}
	if wo.WorkObjectHeader() == nil || wo.Header() == nil {
		return nil, errors.New("work object has no header or no body header")
	}
	again, err := encodeBlock(wo)
	if err != nil {
		return nil, fmt.Errorf("encode work object: %v", err)
	}
	if !bytes.Equal(again, data) {
		return nil, errors.New("block bytes do not encode back to the same bytes")
	}
	return wo, nil
}

// checkBody checks that the work object header binds the body header and that
// the body matches the header roots, as ValidateBody (core/block_validator.go)
// checks them in the context.
func checkBody(wo *types.WorkObject, ctx int) error {
	header := wo.Header()
	if wo.HeaderHash() != header.Hash() {
		return fmt.Errorf("work object header hash %s is not the body header hash %s", wo.HeaderHash().Hex(), header.Hash().Hex())
	}
	if ctx != zoneCtx {
		if len(wo.Transactions()) != 0 || len(wo.OutboundEtxs()) != 0 || len(wo.Uncles()) != 0 {
			return errors.New("a dominant body has transactions, outbound ETXs or uncles")
		}
		if got := types.DeriveSha(wo.Manifest(), trie.NewStackTrie(nil)); got != header.ManifestHash(ctx+1) {
			return fmt.Errorf("manifest root %s, header has %s", got.Hex(), header.ManifestHash(ctx+1).Hex())
		}
		if ctx == primeCtx {
			if got := types.DeriveSha(wo.InterlinkHashes(), trie.NewStackTrie(nil)); got != header.InterlinkRootHash() {
				return fmt.Errorf("interlink root %s, header has %s", got.Hex(), header.InterlinkRootHash().Hex())
			}
		}
		return nil
	}
	if got := types.DeriveSha(wo.Transactions(), trie.NewStackTrie(nil)); got != header.TxHash() {
		return fmt.Errorf("transaction root %s, header has %s", got.Hex(), header.TxHash().Hex())
	}
	if got := types.DeriveSha(types.Transactions(wo.OutboundEtxs()), trie.NewStackTrie(nil)); got != header.OutboundEtxHash() {
		return fmt.Errorf("outbound ETX root %s, header has %s", got.Hex(), header.OutboundEtxHash().Hex())
	}
	if got := types.CalcUncleHash(wo.Uncles()); got != header.UncleHash() {
		return fmt.Errorf("uncle hash %s, header has %s", got.Hex(), header.UncleHash().Hex())
	}
	return nil
}

// checkReceipts decodes stored receipts and checks them against the header
// receipt root, as ValidateState computes it. The storage form has no
// transaction type, so it comes from the block transaction with the receipt's
// transaction hash, as the state processor sets it. The hashes use the zone
// location; for transactions of this zone that is the hash that sender
// recovery gives.
func checkReceipts(wo *types.WorkObject, data []byte) (int, error) {
	protoReceipts := new(types.ProtoReceiptsForStorage)
	if err := proto.Unmarshal(data, protoReceipts); err != nil {
		return 0, fmt.Errorf("decode receipts: %v", err)
	}
	var stored types.ReceiptsForStorage
	if err := stored.ProtoDecode(protoReceipts, zoneLocation); err != nil {
		return 0, fmt.Errorf("decode receipts: %v", err)
	}
	txTypes := make(map[common.Hash]uint8, len(wo.Transactions()))
	for _, tx := range wo.Transactions() {
		h, err := tx.HashWithError(zoneLocation...)
		if err != nil {
			return 0, fmt.Errorf("transaction hash: %v", err)
		}
		txTypes[h] = tx.Type()
	}
	receipts := make(types.Receipts, len(stored))
	for i, r := range stored {
		txType, ok := txTypes[r.TxHash]
		if !ok {
			return 0, fmt.Errorf("receipt %d is for transaction %s, which is not in the block", i, r.TxHash.Hex())
		}
		r.Type = txType
		receipts[i] = (*types.Receipt)(r)
	}
	if got := types.DeriveSha(receipts, trie.NewStackTrie(nil)); got != wo.Header().ReceiptHash() {
		return 0, fmt.Errorf("receipt root of the stored receipts is %s, header has %s", got.Hex(), wo.Header().ReceiptHash().Hex())
	}
	return len(receipts), nil
}

// ---------------------------------------------------------------------------
// Manifest and segment files.

type segmentInfo struct {
	File      string `json:"file"`
	FirstZone uint64 `json:"first_zone"`
	LastZone  uint64 `json:"last_zone"`
	Records   uint64 `json:"records"`
	Sha256    string `json:"sha256"`
}

type manifest struct {
	Format            string        `json:"format"`
	ReferenceRevision string        `json:"reference_revision"`
	Environment       string        `json:"environment"`
	Slice             [2]int        `json:"slice"`
	GenesisHash       string        `json:"genesis_hash"`
	Receipts          bool          `json:"receipts"`
	LastZone          uint64        `json:"last_zone"`
	Segments          []segmentInfo `json:"segments"`
	ExportedUTC       string        `json:"exported_utc"`
}

func segmentFile(first, last uint64) string {
	return fmt.Sprintf("segments/%010d-%010d.qex", first, last)
}

func readManifest(outDir string) (*manifest, error) {
	data, err := os.ReadFile(filepath.Join(outDir, "manifest.json"))
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	m := new(manifest)
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(m); err != nil {
		return nil, fmt.Errorf("manifest.json: %v", err)
	}
	return m, nil
}

// checkManifest checks the manifest fields, and that the segments are
// contiguous from zone height 0 with every segment except the last one full.
func checkManifest(m *manifest) error {
	if m.Format != formatName {
		return fmt.Errorf("manifest format %q, expected %q", m.Format, formatName)
	}
	if m.ReferenceRevision != pinnedRevision {
		return fmt.Errorf("manifest reference_revision %s, expected %s", m.ReferenceRevision, pinnedRevision)
	}
	if m.Environment != environmentName || m.Slice != [2]int{0, 0} {
		return fmt.Errorf("manifest is for %s %v, expected %s [0 0]", m.Environment, m.Slice, environmentName)
	}
	if m.GenesisHash != params.ProgpowColosseumGenesisHash.Hex() {
		return fmt.Errorf("manifest genesis_hash %s is not the mainnet genesis %s", m.GenesisHash, params.ProgpowColosseumGenesisHash.Hex())
	}
	if len(m.Segments) == 0 {
		return errors.New("manifest has no segments")
	}
	next := uint64(0)
	for i, s := range m.Segments {
		if !segmentName.MatchString(s.File) || s.File != segmentFile(s.FirstZone, s.LastZone) {
			return fmt.Errorf("segment %d: file name %q does not match its range %d-%d", i, s.File, s.FirstZone, s.LastZone)
		}
		if s.FirstZone != next || s.LastZone < s.FirstZone {
			return fmt.Errorf("segment %s: the ranges are not contiguous from zone height 0", s.File)
		}
		heights := s.LastZone - s.FirstZone + 1
		if heights > segmentHeights || (heights < segmentHeights && i != len(m.Segments)-1) {
			return fmt.Errorf("segment %s: %d zone heights; only the last segment may have fewer than %d", s.File, heights, segmentHeights)
		}
		if s.Records < heights {
			return fmt.Errorf("segment %s: %d records for %d zone heights", s.File, s.Records, heights)
		}
		if _, err := hex.DecodeString(s.Sha256); err != nil || len(s.Sha256) != 64 {
			return fmt.Errorf("segment %s: bad sha256 %q", s.File, s.Sha256)
		}
		next = s.LastZone + 1
	}
	if m.LastZone != next-1 {
		return fmt.Errorf("manifest last_zone %d, but the segments end at %d", m.LastZone, next-1)
	}
	return nil
}

func writeManifest(outDir string, m *manifest) error {
	m.ExportedUTC = time.Now().UTC().Format("2006-01-02T15:04:05Z")
	data, err := json.MarshalIndent(m, "", "  ")
	if err != nil {
		return err
	}
	path := filepath.Join(outDir, "manifest.json")
	if err := writeFileSync(path+".tmp", append(data, '\n')); err != nil {
		return err
	}
	if err := os.Rename(path+".tmp", path); err != nil {
		return err
	}
	return syncDir(outDir)
}

func writeFileSync(path string, data []byte) error {
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0o644)
	if err != nil {
		return err
	}
	if _, err := f.Write(data); err != nil {
		f.Close()
		return err
	}
	if err := f.Sync(); err != nil {
		f.Close()
		return err
	}
	return f.Close()
}

func syncDir(dir string) error {
	d, err := os.Open(dir)
	if err != nil {
		return err
	}
	defer d.Close()
	if err := d.Sync(); err != nil && !errors.Is(err, syscall.EINVAL) && !errors.Is(err, syscall.ENOTSUP) {
		return err
	}
	return nil
}

func fileSha256(path string) (string, error) {
	f, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer f.Close()
	sum := sha256.New()
	if _, err := io.Copy(sum, f); err != nil {
		return "", err
	}
	return hex.EncodeToString(sum.Sum(nil)), nil
}

// segmentWriter writes one segment to <name>.pending and renames it to
// <name> when it is complete, so a reader never sees a partial segment.
type segmentWriter struct {
	outDir string
	info   segmentInfo
	file   *os.File
	buf    *bufio.Writer
	sum    hash.Hash
	bytes  uint64
	header [recordHeaderLen]byte
	length [4]byte
}

func createSegment(outDir string, first, last uint64) (*segmentWriter, error) {
	name := segmentFile(first, last)
	f, err := os.OpenFile(filepath.Join(outDir, name+".pending"), os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0o644)
	if err != nil {
		return nil, err
	}
	w := &segmentWriter{outDir: outDir, info: segmentInfo{File: name, FirstZone: first, LastZone: last}, file: f, sum: sha256.New()}
	w.buf = bufio.NewWriterSize(io.MultiWriter(f, w.sum), 8<<20)
	if _, err := w.buf.WriteString(segmentMagic); err != nil {
		w.abort()
		return nil, err
	}
	w.bytes = uint64(len(segmentMagic))
	return w, nil
}

func (w *segmentWriter) write(r *record) error {
	if uint64(len(r.block)) > 0xffffffff || uint64(len(r.receipts)) > 0xffffffff {
		return fmt.Errorf("%s: a record field is longer than 4 GiB", r.describe())
	}
	h := w.header[:]
	h[0] = recordKindBlock
	h[1] = byte(r.ctx)
	binary.BigEndian.PutUint64(h[2:10], r.number)
	copy(h[10:42], r.hash[:])
	binary.BigEndian.PutUint32(h[42:46], uint32(len(r.block)))
	binary.BigEndian.PutUint32(w.length[:], uint32(len(r.receipts)))
	for _, part := range [][]byte{h, r.block, w.length[:], r.receipts} {
		if _, err := w.buf.Write(part); err != nil {
			return err
		}
	}
	w.info.Records++
	w.bytes += r.size()
	return nil
}

func (w *segmentWriter) finish() (segmentInfo, error) {
	if err := w.buf.Flush(); err != nil {
		return segmentInfo{}, err
	}
	if err := w.file.Sync(); err != nil {
		return segmentInfo{}, err
	}
	if err := w.file.Close(); err != nil {
		return segmentInfo{}, err
	}
	path := filepath.Join(w.outDir, w.info.File)
	if err := os.Rename(path+".pending", path); err != nil {
		return segmentInfo{}, err
	}
	if err := syncDir(filepath.Dir(path)); err != nil {
		return segmentInfo{}, err
	}
	w.info.Sha256 = hex.EncodeToString(w.sum.Sum(nil))
	return w.info, nil
}

func (w *segmentWriter) abort() {
	w.file.Close()
	os.Remove(filepath.Join(w.outDir, w.info.File+".pending"))
}

// segmentReader reads the records of one segment file.
type segmentReader struct {
	file   *os.File
	r      *bufio.Reader
	header [recordHeaderLen]byte
	offset uint64
}

func openSegment(path string) (*segmentReader, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	sr := &segmentReader{file: f, r: bufio.NewReaderSize(f, 8<<20)}
	magic := make([]byte, len(segmentMagic))
	if _, err := io.ReadFull(sr.r, magic); err != nil || string(magic) != segmentMagic {
		f.Close()
		return nil, fmt.Errorf("%s: no %q magic", path, segmentMagic)
	}
	sr.offset = uint64(len(segmentMagic))
	return sr, nil
}

func (sr *segmentReader) close() { sr.file.Close() }

// next returns the next record, or io.EOF at the end of the file. With body
// false it skips the block and receipts bytes.
func (sr *segmentReader) next(body bool) (*record, error) {
	h := sr.header[:]
	if _, err := io.ReadFull(sr.r, h[:1]); err == io.EOF {
		return nil, io.EOF
	} else if err != nil {
		return nil, err
	}
	truncated := func() error { return fmt.Errorf("truncated record at offset %d", sr.offset) }
	if _, err := io.ReadFull(sr.r, h[1:]); err != nil {
		return nil, truncated()
	}
	if h[0] != recordKindBlock {
		return nil, fmt.Errorf("unknown record kind %d at offset %d", h[0], sr.offset)
	}
	r := &record{ctx: int(h[1]), number: binary.BigEndian.Uint64(h[2:10])}
	copy(r.hash[:], h[10:42])
	if r.ctx > zoneCtx {
		return nil, fmt.Errorf("unknown context %d at offset %d", r.ctx, sr.offset)
	}
	field := func(n uint32) ([]byte, error) {
		if !body {
			_, err := sr.r.Discard(int(n))
			return nil, err
		}
		data := make([]byte, n)
		_, err := io.ReadFull(sr.r, data)
		return data, err
	}
	blockLen := binary.BigEndian.Uint32(h[42:46])
	var err error
	if r.block, err = field(blockLen); err != nil {
		return nil, truncated()
	}
	var length [4]byte
	if _, err := io.ReadFull(sr.r, length[:]); err != nil {
		return nil, truncated()
	}
	receiptsLen := binary.BigEndian.Uint32(length[:])
	if r.receipts, err = field(receiptsLen); err != nil {
		return nil, truncated()
	}
	sr.offset += uint64(recordHeaderLen) + uint64(blockLen) + 4 + uint64(receiptsLen)
	return r, nil
}

// ---------------------------------------------------------------------------
// Export.

type exporter struct {
	dbs      [3]ethdb.Database
	outDir   string
	receipts bool
	minFree  uint64
	checker  *chainChecker
	started  time.Time
}

func openContext(dbRoot string, ctx int) (ethdb.Database, error) {
	dir := filepath.Join(dbRoot, contextDirs[ctx], "go-quai", "chaindata")
	if _, err := os.Stat(filepath.Join(dir, "CURRENT")); err != nil {
		return nil, fmt.Errorf("no %s database at %s: %v", contextNames[ctx], dir, err)
	}
	// The node's layout and options (node.OpenDatabaseWithFreezer with the
	// default ancient directory and the detected database type), read-only.
	return rawdb.Open(rawdb.OpenOptions{
		Directory:         dir,
		AncientsDirectory: filepath.Join(dir, "ancient"),
		Namespace:         "eth/db/chaindata/",
		Cache:             contextCacheMB[ctx],
		Handles:           512,
		ReadOnly:          true,
	}, ctx, log.Global, contextLocations[ctx])
}

func (e *exporter) checkFreeSpace() error {
	var st syscall.Statfs_t
	if err := syscall.Statfs(e.outDir, &st); err != nil {
		return err
	}
	if free := uint64(st.Bavail) * uint64(st.Bsize); free < e.minFree {
		return fmt.Errorf("only %.1f GiB free on the output volume, below the %.1f GiB limit (the node's disk guard stops the node at 25 GiB)", float64(free)/(1<<30), float64(e.minFree)/(1<<30))
	}
	return nil
}

// lockDir takes an exclusive lock on the export directory itself, so that
// two exporters never write one export.
func lockDir(outDir string) (*os.File, error) {
	f, err := os.Open(outDir)
	if err != nil {
		return nil, err
	}
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		f.Close()
		return nil, fmt.Errorf("another export is writing %s: %v", outDir, err)
	}
	return f, nil
}

func runExport(dbRoot, outDir string, maxZone int64, receipts bool, minFreeGB uint64) error {
	// scripts/mainnet/go_node.sh keeps go-quai.pid next to the node's db/.
	// Opening that database directly could let LevelDB repair write to it.
	if _, err := os.Stat(filepath.Join(filepath.Dir(filepath.Clean(dbRoot)), "go-quai.pid")); err == nil {
		return fmt.Errorf("%s is the node's own database; export from an APFS clone (scripts/mainnet/export.sh)", dbRoot)
	}
	if err := os.MkdirAll(filepath.Join(outDir, "segments"), 0o755); err != nil {
		return err
	}
	lock, err := lockDir(outDir)
	if err != nil {
		return err
	}
	defer lock.Close()

	e := &exporter{outDir: outDir, receipts: receipts, minFree: minFreeGB << 30, checker: newChainChecker(receipts), started: time.Now()}
	for ctx := primeCtx; ctx <= zoneCtx; ctx++ {
		db, err := openContext(dbRoot, ctx)
		if err != nil {
			return err
		}
		defer db.Close()
		e.dbs[ctx] = db
	}
	genesis := params.ProgpowColosseumGenesisHash
	for ctx := primeCtx; ctx <= zoneCtx; ctx++ {
		if got := rawdb.ReadCanonicalHash(e.dbs[ctx], 0); got != genesis {
			return fmt.Errorf("the %s database genesis is %s, not the mainnet genesis %s", contextNames[ctx], got.Hex(), genesis.Hex())
		}
	}

	// The zone head is the last block whose state go-quai has processed:
	// SetCurrentHeader writes the head block hash after AppendBlock has
	// written the block state and receipts.
	zone := e.dbs[zoneCtx]
	headHash := rawdb.ReadHeadBlockHash(zone)
	headNumber := rawdb.ReadHeaderNumber(zone, headHash)
	if headHash == (common.Hash{}) || headNumber == nil {
		return errors.New("the zone database has no head block")
	}
	if rawdb.ReadCanonicalHash(zone, *headNumber) != headHash {
		return fmt.Errorf("the zone head %s is not canonical at height %d", headHash.Hex(), *headNumber)
	}
	end := *headNumber
	if maxZone >= 0 && uint64(maxZone) < end {
		end = uint64(maxZone)
	}

	// Resume: keep the complete segments of an existing export and write the
	// last, incomplete segment again.
	old, err := readManifest(outDir)
	if err != nil {
		return err
	}
	var kept []segmentInfo
	if old != nil {
		if err := checkManifest(old); err != nil {
			return fmt.Errorf("existing export: %v", err)
		}
		if old.Receipts != receipts {
			return fmt.Errorf("the existing export has receipts=%v; this run has receipts=%v", old.Receipts, receipts)
		}
		if end < old.LastZone {
			return fmt.Errorf("the export would end at zone height %d, below the existing export (%d); use a newer database copy", end, old.LastZone)
		}
		for _, s := range old.Segments {
			if s.LastZone-s.FirstZone+1 < segmentHeights {
				break
			}
			sum, err := fileSha256(filepath.Join(outDir, s.File))
			if err != nil {
				return fmt.Errorf("existing export: %v", err)
			}
			if sum != s.Sha256 {
				return fmt.Errorf("existing export: %s has sha256 %s, the manifest has %s", s.File, sum, s.Sha256)
			}
			kept = append(kept, s)
		}
	}
	if err := removeUnlisted(outDir, old); err != nil {
		return err
	}
	first := uint64(0)
	if len(kept) > 0 {
		if err := e.resume(kept); err != nil {
			return err
		}
		first = kept[len(kept)-1].LastZone + 1
	}
	emit(map[string]interface{}{"event": "start", "zone_head": *headNumber, "zone_head_hash": headHash.Hex(), "export_from": first, "export_to": end, "kept_segments": len(kept)})

	m := &manifest{Format: formatName, ReferenceRevision: pinnedRevision, Environment: environmentName, Slice: [2]int{0, 0}, GenesisHash: genesis.Hex(), Receipts: receipts, Segments: kept}
	for segFirst := first; segFirst <= end; segFirst += segmentHeights {
		segLast := segFirst + segmentHeights - 1
		if segLast > end {
			segLast = end
		}
		if err := e.checkFreeSpace(); err != nil {
			return err
		}
		info, err := e.writeSegment(segFirst, segLast)
		if err != nil {
			return err
		}
		m.Segments = append(m.Segments, info)
		m.LastZone = segLast
		if err := writeManifest(outDir, m); err != nil {
			return err
		}
		if err := removeUnlisted(outDir, m); err != nil {
			return err
		}
		emit(map[string]interface{}{"event": "segment", "file": info.File, "first_zone": info.FirstZone, "last_zone": info.LastZone, "records": info.Records, "sha256": info.Sha256})
	}
	if first > end {
		emit(map[string]interface{}{"event": "up_to_date", "last_zone": end})
	}
	if err := e.checkTip(end, *headNumber); err != nil {
		return err
	}
	return summarize(outDir, e.checker, map[string]interface{}{"zone_head": *headNumber, "zone_head_hash": headHash.Hex(), "exported_from": first, "elapsed_s": int(time.Since(e.started).Seconds())})
}

// removeUnlisted removes segment files that the manifest does not list:
// .pending files and segments of an interrupted run, and the incomplete
// segment that a resumed run has replaced.
func removeUnlisted(outDir string, m *manifest) error {
	listed := map[string]bool{}
	if m != nil {
		for _, s := range m.Segments {
			listed[s.File] = true
		}
	}
	entries, err := os.ReadDir(filepath.Join(outDir, "segments"))
	if err != nil {
		return err
	}
	for _, entry := range entries {
		name := "segments/" + entry.Name()
		base := name
		if filepath.Ext(name) == ".pending" {
			base = name[:len(name)-len(".pending")]
		}
		if listed[name] || entry.IsDir() || !segmentName.MatchString(base) {
			continue
		}
		if err := os.Remove(filepath.Join(outDir, name)); err != nil {
			return err
		}
	}
	return nil
}

// resume restores the chain state at the end of the kept segments, and checks
// that their last blocks are still canonical in the database.
func (e *exporter) resume(kept []segmentInfo) error {
	var tips [3]tip
	for i := len(kept) - 1; i >= 0 && !(tips[0].set && tips[1].set && tips[2].set); i-- {
		sr, err := openSegment(filepath.Join(e.outDir, kept[i].File))
		if err != nil {
			return err
		}
		var found [3]tip
		for {
			r, err := sr.next(false)
			if err == io.EOF {
				break
			}
			if err != nil {
				sr.close()
				return fmt.Errorf("%s: %v", kept[i].File, err)
			}
			found[r.ctx] = tip{number: r.number, hash: r.hash, set: true}
		}
		sr.close()
		for ctx := range tips {
			if !tips[ctx].set {
				tips[ctx] = found[ctx]
			}
		}
	}
	last := kept[len(kept)-1]
	if !tips[zoneCtx].set || tips[zoneCtx].number != last.LastZone {
		return fmt.Errorf("existing export: %s does not end with zone height %d", last.File, last.LastZone)
	}
	for ctx := primeCtx; ctx <= zoneCtx; ctx++ {
		if !tips[ctx].set {
			return fmt.Errorf("the existing export has no %s record", contextNames[ctx])
		}
		if got := rawdb.ReadCanonicalHash(e.dbs[ctx], tips[ctx].number); got != tips[ctx].hash {
			return fmt.Errorf("the last exported %s block %d (%s) is no longer canonical (the database has %s): remove %s and later segments from the manifest and export again",
				contextNames[ctx], tips[ctx].number, tips[ctx].hash.Hex(), got.Hex(), last.File)
		}
	}
	e.checker.last = tips
	e.checker.nextZone = last.LastZone + 1
	emit(map[string]interface{}{"event": "resume", "kept_segments": len(kept), "last_prime": tips[primeCtx].number, "last_region": tips[regionCtx].number, "last_zone": tips[zoneCtx].number})
	return nil
}

func (e *exporter) writeSegment(first, last uint64) (segmentInfo, error) {
	w, err := createSegment(e.outDir, first, last)
	if err != nil {
		return segmentInfo{}, err
	}
	done := false
	defer func() {
		if !done {
			w.abort()
		}
	}()
	for z := first; z <= last; z++ {
		records, err := e.collect(z)
		if err != nil {
			return segmentInfo{}, err
		}
		for _, r := range records {
			if err := e.checker.check(r); err != nil {
				return segmentInfo{}, err
			}
			if err := w.write(r); err != nil {
				return segmentInfo{}, err
			}
		}
		if z%progressEvery == 0 && z != first {
			if err := e.checkFreeSpace(); err != nil {
				return segmentInfo{}, err
			}
			emit(map[string]interface{}{"event": "progress", "zone": z, "segment_records": w.info.Records, "segment_bytes": w.bytes, "elapsed_s": int(time.Since(e.started).Seconds())})
		}
	}
	info, err := w.finish()
	if err != nil {
		return segmentInfo{}, err
	}
	done = true
	return info, nil
}

// collect reads the records of one zone height: the prime and region work
// objects when the zone block is canonical in those contexts, then the zone
// work object and its stored receipts.
func (e *exporter) collect(z uint64) ([]*record, error) {
	zone := e.dbs[zoneCtx]
	h := rawdb.ReadCanonicalHash(zone, z)
	if h == (common.Hash{}) {
		return nil, fmt.Errorf("zone height %d has no canonical hash", z)
	}
	zoneWo, err := e.readBlock(zoneCtx, z, h)
	if err != nil {
		return nil, err
	}
	var records []*record
	for ctx := primeCtx; ctx <= regionCtx; ctx++ {
		n := zoneWo.NumberU64(ctx)
		if rawdb.ReadCanonicalHash(e.dbs[ctx], n) != h {
			if z == 0 {
				return nil, fmt.Errorf("the %s genesis is not the zone genesis %s", contextNames[ctx], h.Hex())
			}
			// go-quai stores a block in a dominant database only when it
			// appends it there as a block of that order; on a consistent
			// chain such a block is canonical in that context.
			if stored := rawdb.ReadHeaderNumber(e.dbs[ctx], h); stored != nil {
				return nil, fmt.Errorf("zone block %d (%s) is stored in the %s database at %d but is not canonical there: the database copy is not consistent", z, h.Hex(), contextNames[ctx], *stored)
			}
			continue
		}
		wo, err := e.readBlock(ctx, n, h)
		if err != nil {
			return nil, err
		}
		data, err := encodeBlock(wo)
		if err != nil {
			return nil, fmt.Errorf("%s block %d (%s): encode: %v", contextNames[ctx], n, h.Hex(), err)
		}
		records = append(records, &record{ctx: ctx, number: n, hash: h, block: data})
	}
	data, err := encodeBlock(zoneWo)
	if err != nil {
		return nil, fmt.Errorf("zone block %d (%s): encode: %v", z, h.Hex(), err)
	}
	zoneRecord := &record{ctx: zoneCtx, number: z, hash: h, block: data}
	stored, found, err := rawdb.NativeExportReceipts(zone, h, z)
	if err != nil {
		return nil, fmt.Errorf("zone block %d (%s): read receipts: %v", z, h.Hex(), err)
	}
	if !found {
		return nil, fmt.Errorf("zone block %d (%s) has no receipts entry (key %x)", z, h.Hex(), rawdb.NativeExportReceiptsKey(z, h))
	}
	if e.receipts {
		zoneRecord.receipts = stored
	}
	return append(records, zoneRecord), nil
}

// readBlock reads a canonical work object of a context database and checks
// that the hash recomputed from it is the canonical hash.
func (e *exporter) readBlock(ctx int, number uint64, h common.Hash) (*types.WorkObject, error) {
	wo := rawdb.ReadWorkObject(e.dbs[ctx], number, h, types.BlockObject)
	if wo == nil {
		return nil, fmt.Errorf("%s block %d (%s) is missing from the database", contextNames[ctx], number, h.Hex())
	}
	if got := wo.Hash(); got != h {
		return nil, fmt.Errorf("%s block %d: the recomputed hash %s is not the canonical hash %s", contextNames[ctx], number, got.Hex(), h.Hex())
	}
	return wo, nil
}

// checkTip checks the last exported zone block from the other side. Its
// child, when the zone chain has one, must point at the last exported zone,
// region and prime blocks. The next prime and region blocks, when those
// databases have them, must be zone [0 0] blocks above the exported range:
// a block from another zone is not coincident with the zone chain.
func (e *exporter) checkTip(end, head uint64) error {
	c := e.checker
	if end < head {
		h := rawdb.ReadCanonicalHash(e.dbs[zoneCtx], end+1)
		child, err := e.readBlock(zoneCtx, end+1, h)
		if err != nil {
			return err
		}
		what := fmt.Sprintf("zone block %d (%s) after the export", end+1, h.Hex())
		if child.ParentHash(zoneCtx) != c.last[zoneCtx].hash {
			return fmt.Errorf("%s: its parent is not the last exported zone block %s", what, c.last[zoneCtx].hash.Hex())
		}
		if err := c.checkDominantParents(child, what); err != nil {
			return err
		}
	}
	for ctx := primeCtx; ctx <= regionCtx; ctx++ {
		n := c.last[ctx].number + 1
		h := rawdb.ReadCanonicalHash(e.dbs[ctx], n)
		if h == (common.Hash{}) {
			continue
		}
		wo, err := e.readBlock(ctx, n, h)
		if err != nil {
			return err
		}
		if !wo.Location().Equal(zoneLocation) {
			return fmt.Errorf("%s block %d (%s) is from zone %v and not coincident with a zone [0 0] block; QEX1 does not cover it", contextNames[ctx], n, h.Hex(), wo.Location())
		}
		if z := wo.NumberU64(zoneCtx); z <= end {
			return fmt.Errorf("%s block %d (%s) has zone height %d inside the exported range, but the zone chain has another block there", contextNames[ctx], n, h.Hex(), z)
		}
	}
	return nil
}

// summarize prints the summary of an export directory as the last JSON line.
func summarize(outDir string, c *chainChecker, extra map[string]interface{}) error {
	m, err := readManifest(outDir)
	if err != nil {
		return err
	}
	if m == nil {
		return errors.New("no manifest")
	}
	var records, size uint64
	files := []map[string]interface{}{}
	for _, s := range m.Segments {
		st, err := os.Stat(filepath.Join(outDir, s.File))
		if err != nil {
			return err
		}
		records += s.Records
		size += uint64(st.Size())
		files = append(files, map[string]interface{}{"file": s.File, "first_zone": s.FirstZone, "last_zone": s.LastZone, "records": s.Records, "bytes": st.Size(), "sha256": s.Sha256})
	}
	blocks := [3]uint64{c.last[primeCtx].number + 1, c.last[regionCtx].number + 1, c.last[zoneCtx].number + 1}
	if c.last[zoneCtx].number != m.LastZone || records != blocks[0]+blocks[1]+blocks[2] {
		return fmt.Errorf("the manifest has %d records to zone height %d; the chains have %d prime, %d region and %d zone blocks to zone height %d",
			records, m.LastZone, blocks[0], blocks[1], blocks[2], c.last[zoneCtx].number)
	}
	summary := map[string]interface{}{
		"event":          "complete",
		"passed":         true,
		"format":         m.Format,
		"genesis_hash":   m.GenesisHash,
		"receipts":       m.Receipts,
		"last_zone":      m.LastZone,
		"last_zone_hash": c.last[zoneCtx].hash.Hex(),
		"last_region":    c.last[regionCtx].number,
		"last_prime":     c.last[primeCtx].number,
		"records":        records,
		"bytes":          size,
		"prime_records":  blocks[0],
		"region_records": blocks[1],
		"zone_records":   blocks[2],
		"exported_utc":   m.ExportedUTC,
		"segments":       files,
		"checked": map[string]interface{}{
			"prime_records": c.checked[primeCtx], "region_records": c.checked[regionCtx], "zone_records": c.checked[zoneCtx],
			"transactions": c.transactions, "receipts": c.receiptCount, "receipt_bytes": c.receiptBytes,
			"expansion_numbers": c.expansion,
		},
	}
	for k, v := range extra {
		summary[k] = v
	}
	emit(summary)
	return nil
}

// ---------------------------------------------------------------------------
// Verify.

// verifyExport checks a finished export from its files alone: the manifest,
// segment names and hashes, the record layout and order, and every record
// with the same chain rules as the exporter.
func verifyExport(outDir string) error {
	m, err := readManifest(outDir)
	if err != nil {
		return err
	}
	if m == nil {
		return fmt.Errorf("%s has no manifest.json", outDir)
	}
	if err := checkManifest(m); err != nil {
		return err
	}
	listed := map[string]bool{}
	for _, s := range m.Segments {
		listed[s.File] = true
	}
	entries, err := os.ReadDir(filepath.Join(outDir, "segments"))
	if err != nil {
		return err
	}
	for _, entry := range entries {
		if !listed["segments/"+entry.Name()] && entry.Name()[0] != '.' {
			return fmt.Errorf("segments/%s is not in the manifest", entry.Name())
		}
	}
	c := newChainChecker(m.Receipts)
	started := time.Now()
	for _, s := range m.Segments {
		path := filepath.Join(outDir, s.File)
		sum, err := fileSha256(path)
		if err != nil {
			return err
		}
		if sum != s.Sha256 {
			return fmt.Errorf("%s: sha256 %s, the manifest has %s", s.File, sum, s.Sha256)
		}
		sr, err := openSegment(path)
		if err != nil {
			return err
		}
		if c.nextZone != s.FirstZone {
			sr.close()
			return fmt.Errorf("%s: the records before it end at zone height %d", s.File, c.nextZone-1)
		}
		var records uint64
		for {
			r, err := sr.next(true)
			if err == io.EOF {
				break
			}
			if err == nil {
				err = c.check(r)
			}
			if err != nil {
				sr.close()
				return fmt.Errorf("%s: %v", s.File, err)
			}
			records++
		}
		sr.close()
		if err := c.complete(); err != nil {
			return fmt.Errorf("%s: %v", s.File, err)
		}
		if c.nextZone != s.LastZone+1 {
			return fmt.Errorf("%s: the records end at zone height %d, the manifest has %d", s.File, c.nextZone-1, s.LastZone)
		}
		if records != s.Records {
			return fmt.Errorf("%s: %d records, the manifest has %d", s.File, records, s.Records)
		}
		emit(map[string]interface{}{"event": "verified_segment", "file": s.File, "records": records, "elapsed_s": int(time.Since(started).Seconds())})
	}
	return summarize(outDir, c, map[string]interface{}{"event": "verified", "elapsed_s": int(time.Since(started).Seconds())})
}
