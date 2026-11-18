// Command quai-replay-bench runs exported go-quai mainnet blocks (QEX1,
// docs/mainnet-export-format.md) through go-quai v0.57.0's own block import
// path, from genesis, on a fresh database, so that its cost can be compared
// with the Rust `quai-node replay --state` (ROADMAP.md, Phase 4).
//
// It is an explicit offline Go oracle. scripts/benchmark_replay.py compiles it
// into the pinned go-quai module with `go build -overlay` (together with
// core-bench.go and progpow-bench.go) and never into the Rust build.
//
//	quai-replay-bench replay -export DIR -datadir DIR -params DIR [-to N]
//	quai-replay-bench powbench -export DIR -from A -to B [-out FILE] [-variants]
//
// replay builds the prime, region-0 and zone-0-0 contexts as quai.New does
// for a node of slice [0 0] on colosseum with the default flags (the
// databases, the genesis with its allocations, the ProgPoW and KAWPOW
// engines, core.NewCore) and wires them as the hierarchical coordinator does.
// For each zone height it stores the bodies of the height's records, appends
// the block in its own context (the dominant contexts append their
// subordinates) and sets the new heads; the zone head runs the state
// processor. See core-bench.go for the exact steps. Every block and
// workshare header gets go-quai's full proof-of-work check; the engines are
// wrapped only to add up the time spent in their proof-of-work calls.
// The loggers write warnings only, to <datadir>/go-quai-<context>.log.
//
// powbench recomputes the ProgPoW proof of every block header and workshare
// in a range with the light kernel, without caches, and prints one line per
// input. With -variants it adds mutated inputs (nonce, seal, claimed mix) and
// the same seal and nonce in later epochs. The Rust example `pow-bench`
// prints the same lines.
package main

import (
	"bufio"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"math/big"
	"os"
	"path/filepath"
	"runtime"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/common/fdlimit"
	"github.com/dominant-strategies/go-quai/consensus"
	"github.com/dominant-strategies/go-quai/consensus/kawpow"
	"github.com/dominant-strategies/go-quai/consensus/progpow"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/core/vm"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"github.com/dominant-strategies/go-quai/quai/quaiconfig"
	"github.com/sirupsen/logrus"
	"google.golang.org/protobuf/proto"
)

// Set with -ldflags "-X main.referenceRevision=<pin>".
var referenceRevision string

const pinnedRevision = "33db82f2891b267ba898ca38bd2dd8bf75cbb072"

var (
	contextNames     = [3]string{"prime", "region", "zone"}
	contextLocations = [3]common.Location{{}, {0}, {0, 0}}
)

func fatalf(format string, args ...interface{}) {
	fmt.Fprintf(os.Stderr, "quai-replay-bench: "+format+"\n", args...)
	os.Exit(1)
}

func main() {
	if referenceRevision != pinnedRevision {
		fatalf("built without the pinned reference revision")
	}
	if len(os.Args) < 2 {
		fatalf("usage: quai-replay-bench replay|powbench [flags]")
	}
	log.Global.SetOutput(os.Stderr)
	log.Global.SetLevel(logrus.WarnLevel)
	switch os.Args[1] {
	case "replay":
		replay(os.Args[2:])
	case "powbench":
		powbench(os.Args[2:])
	default:
		fatalf("unknown mode %q", os.Args[1])
	}
}

// ---------------------------------------------------------------------------
// QEX1 reading (docs/mainnet-export-format.md).

type segment struct {
	File      string `json:"file"`
	FirstZone uint64 `json:"first_zone"`
	LastZone  uint64 `json:"last_zone"`
	Records   uint64 `json:"records"`
	Sha256    string `json:"sha256"`
	Pruned    bool   `json:"pruned"`
}

type manifest struct {
	Format      string    `json:"format"`
	Environment string    `json:"environment"`
	GenesisHash string    `json:"genesis_hash"`
	Receipts    bool      `json:"receipts"`
	LastZone    uint64    `json:"last_zone"`
	Segments    []segment `json:"segments"`
}

type record struct {
	ctx    int
	number uint64
	hash   common.Hash
	block  []byte
}

// height holds the records of one zone height: [prime, region, zone].
type height struct {
	zone    uint64
	records [3]*record
}

type exportReader struct {
	dir      string
	m        manifest
	seg      int
	file     *os.File
	r        *bufio.Reader
	pending  *record
	zone     uint64
	from, to uint64
}

func openExport(dir string, from, to uint64) (*exportReader, error) {
	data, err := os.ReadFile(filepath.Join(dir, "manifest.json"))
	if err != nil {
		return nil, err
	}
	var m manifest
	if err := json.Unmarshal(data, &m); err != nil {
		return nil, err
	}
	if m.Format != "QEX1" || m.Environment != "colosseum" {
		return nil, fmt.Errorf("export %s is %s/%s, not QEX1/colosseum", dir, m.Format, m.Environment)
	}
	if to > m.LastZone {
		to = m.LastZone
	}
	return &exportReader{dir: dir, m: m, seg: -1, from: from, to: to}, nil
}

func (e *exportReader) openNext() error {
	if e.file != nil {
		e.file.Close()
		e.file = nil
	}
	for {
		e.seg++
		if e.seg >= len(e.m.Segments) {
			return io.EOF
		}
		s := e.m.Segments[e.seg]
		if s.LastZone < e.from {
			continue
		}
		if s.FirstZone > e.to {
			return io.EOF
		}
		if s.Pruned {
			return fmt.Errorf("segment %s is pruned", s.File)
		}
		f, err := os.Open(filepath.Join(e.dir, s.File))
		if err != nil {
			return err
		}
		e.file, e.r = f, bufio.NewReaderSize(f, 8<<20)
		magic := make([]byte, 5)
		if _, err := io.ReadFull(e.r, magic); err != nil || string(magic) != "QEX1\n" {
			return fmt.Errorf("%s: no QEX1 magic", s.File)
		}
		return nil
	}
}

func (e *exportReader) readRecord() (*record, error) {
	if e.file == nil {
		if err := e.openNext(); err != nil {
			return nil, err
		}
	}
	var h [46]byte
	if _, err := io.ReadFull(e.r, h[:]); err == io.EOF {
		if err := e.openNext(); err != nil {
			return nil, err
		}
		return e.readRecord()
	} else if err != nil {
		return nil, err
	}
	if h[0] != 1 || h[1] > 2 {
		return nil, fmt.Errorf("bad record header kind %d context %d", h[0], h[1])
	}
	r := &record{ctx: int(h[1]), number: binary.BigEndian.Uint64(h[2:10])}
	copy(r.hash[:], h[10:42])
	r.block = make([]byte, binary.BigEndian.Uint32(h[42:46]))
	if _, err := io.ReadFull(e.r, r.block); err != nil {
		return nil, err
	}
	var l [4]byte
	if _, err := io.ReadFull(e.r, l[:]); err != nil {
		return nil, err
	}
	if _, err := e.r.Discard(int(binary.BigEndian.Uint32(l[:]))); err != nil {
		return nil, err
	}
	return r, nil
}

// next returns the next zone height in [from, to], or io.EOF.
func (e *exportReader) next() (*height, error) {
	for {
		var h height
		for {
			r := e.pending
			e.pending = nil
			if r == nil {
				var err error
				if r, err = e.readRecord(); err != nil {
					return nil, err
				}
			}
			if h.records[r.ctx] != nil || (r.ctx < 2 && h.records[2] != nil) {
				return nil, errors.New("records out of order")
			}
			h.records[r.ctx] = r
			if r.ctx == 2 {
				h.zone = r.number
				break
			}
		}
		if h.zone > e.to {
			return nil, io.EOF
		}
		if h.zone >= e.from {
			return &h, nil
		}
	}
}

func decodeBlock(data []byte, ctx int) (*types.WorkObject, error) {
	protoWo := new(types.ProtoWorkObject)
	if err := proto.Unmarshal(data, protoWo); err != nil {
		return nil, err
	}
	wo := new(types.WorkObject)
	if err := wo.ProtoDecode(protoWo, contextLocations[ctx], types.BlockObject); err != nil {
		return nil, err
	}
	return wo, nil
}

// ---------------------------------------------------------------------------
// replay

// powTime adds up the wall time of the engines' proof-of-work calls.
type powTime struct {
	nanos atomic.Int64
	calls atomic.Uint64
}

// timedEngine forwards every call to the go-quai engine unchanged and adds
// the time of ComputePowHash and ComputePowLight to spent.
type timedEngine struct {
	consensus.Engine
	spent *powTime
}

func (t timedEngine) ComputePowHash(h *types.WorkObjectHeader) (common.Hash, error) {
	start := time.Now()
	defer func() { t.spent.nanos.Add(int64(time.Since(start))); t.spent.calls.Add(1) }()
	return t.Engine.ComputePowHash(h)
}

func (t timedEngine) ComputePowLight(h *types.WorkObjectHeader) (common.Hash, common.Hash) {
	start := time.Now()
	defer func() { t.spent.nanos.Add(int64(time.Since(start))); t.spent.calls.Add(1) }()
	return t.Engine.ComputePowLight(h)
}

type contextNode struct {
	core *core.Core
	db   interface{ Close() error }
}

// genesisNonce is utils.GetGenesisNonce with the flag's default value.
func genesisNonce() (uint64, []byte) {
	nonceBytes := common.FromHex("23621466532946281564673705261963422")
	return binary.BigEndian.Uint64(nonceBytes[:8]), nonceBytes[8:]
}

// newContext follows quai.New for one context with the configuration that
// utils.SetQuaiConfig gives a colosseum node of slice [0 0] with the default
// flags.
func newContext(ctx int, datadir, paramsDir string, spent *powTime) (*contextNode, error) {
	location := contextLocations[ctx]
	logFile, err := os.Create(filepath.Join(datadir, "go-quai-"+contextNames[ctx]+".log"))
	if err != nil {
		return nil, err
	}
	logger := logrus.New()
	logger.SetOutput(logFile)
	logger.SetLevel(logrus.WarnLevel)

	cfg := quaiconfig.Defaults
	cfg.NodeLocation = location
	cfg.SlicesRunning = []common.Location{{0, 0}}
	cfg.ConsensusEngine = "progpow"
	cfg.GenesisNonce, cfg.GenesisExtra = genesisNonce()
	cfg.PowConfig.DurationLimit = params.DurationLimit
	cfg.PowConfig.GasCeil = params.ColosseumGasCeil
	cfg.PowConfig.MinDifficulty = new(big.Int).Div(core.DefaultColosseumGenesisBlock(cfg.ConsensusEngine, cfg.GenesisNonce, cfg.GenesisExtra).Difficulty, common.Big2)
	cfg.NetworkId = 1
	cfg.Genesis = core.DefaultColosseumGenesisBlock(cfg.ConsensusEngine, cfg.GenesisNonce, cfg.GenesisExtra)
	cfg.DefaultGenesisHash = params.ProgpowColosseumGenesisHash
	cfg.Genesis.AllocHash = params.AllocHash
	if ctx == common.ZONE_CTX {
		cfg.GenesisAllocs, err = params.VerifyGenesisAllocs(filepath.Join(paramsDir, "genesis_alloc.json"), cfg.Genesis.AllocHash)
		if err != nil {
			return nil, err
		}
		if cfg.ForfeitureAddresses, err = params.LoadForfeitureAddresses(filepath.Join(paramsDir, "forfeiture_addresses.json")); err != nil {
			return nil, err
		}
		if cfg.FounderForfeitureAddresses, err = params.LoadForfeitureAddresses(filepath.Join(paramsDir, "founder_forfeiture_addresses.json")); err != nil {
			return nil, err
		}
	}
	cfg.Genesis.Config.Location = location
	cfg.Miner.WorkShareThreshold = params.WorkSharesThresholdDiff + 10
	cfg.TxPool.Journal = filepath.Join(datadir, contextNames[ctx]+"-transactions.rlp")
	limit, err := fdlimit.Maximum()
	if err != nil {
		return nil, err
	}
	raised, err := fdlimit.Raise(uint64(limit))
	if err != nil {
		return nil, err
	}
	cfg.DatabaseHandles = int(raised / 2)

	// quai.New
	dir := filepath.Join(datadir, contextNames[ctx], "chaindata")
	db, err := rawdb.Open(rawdb.OpenOptions{
		Directory:         dir,
		AncientsDirectory: filepath.Join(dir, "ancient"),
		Namespace:         "eth/db/chaindata/",
		Cache:             cfg.DatabaseCache,
		Handles:           cfg.DatabaseHandles,
	}, ctx, logger, location)
	if err != nil {
		return nil, err
	}
	chainConfig, genesisHash, err := core.SetupGenesisBlockWithOverride(db, cfg.Genesis, cfg.GenesisNonce, cfg.GenesisExtra, cfg.NodeLocation, 0, logger)
	if err != nil {
		return nil, err
	}
	if genesisHash != params.ProgpowColosseumGenesisHash {
		return nil, fmt.Errorf("%s genesis %s is not the colosseum genesis", contextNames[ctx], genesisHash.Hex())
	}
	newChainConfig := params.ChainConfig{
		ChainID:         chainConfig.ChainID,
		ConsensusEngine: chainConfig.ConsensusEngine,
		Blake3Pow:       chainConfig.Blake3Pow,
		Progpow:         chainConfig.Progpow,
		Location:        chainConfig.Location,
	}
	chainConfig = &newChainConfig
	chainConfig.Location = cfg.NodeLocation
	chainConfig.DefaultGenesisHash = cfg.DefaultGenesisHash
	chainConfig.IndexAddressUtxos = cfg.IndexAddressUtxos
	chainConfig.TelemetryEnabled = cfg.TelemetryEnabled

	powConfig := cfg.PowConfig
	powConfig.NodeLocation = cfg.NodeLocation
	powConfig.NotifyFull = cfg.Miner.NotifyFull
	powConfig.GenAllocs = cfg.GenesisAllocs
	powConfig.ForfeitureAddresses = cfg.ForfeitureAddresses
	powConfig.FounderForfeitureAddresses = cfg.FounderForfeitureAddresses
	engineConfig := params.PowConfig{
		PowMode:                    powConfig.PowMode,
		NotifyFull:                 powConfig.NotifyFull,
		DurationLimit:              powConfig.DurationLimit,
		NodeLocation:               location,
		GasCeil:                    powConfig.GasCeil,
		GenAllocs:                  powConfig.GenAllocs,
		ForfeitureAddresses:        powConfig.ForfeitureAddresses,
		FounderForfeitureAddresses: powConfig.FounderForfeitureAddresses,
		MinDifficulty:              powConfig.MinDifficulty,
		WorkShareThreshold:         powConfig.WorkShareThreshold,
	}
	engines := make([]consensus.Engine, params.TotalPowEngines)
	pp := progpow.New(engineConfig, cfg.Miner.Notify, cfg.Miner.Noverify, logger)
	pp.SetThreads(-1)
	engines[types.Progpow] = timedEngine{Engine: pp, spent: spent}
	kawpowConfig := engineConfig
	kawpowConfig.PowMode = 0
	kp := kawpow.New(kawpowConfig, cfg.Miner.Notify, cfg.Miner.Noverify, logger)
	kp.SetThreads(-1)
	engines[types.Kawpow] = timedEngine{Engine: kp, spent: spent}

	if v := rawdb.ReadDatabaseVersion(db); v == nil || *v < core.BlockChainVersion {
		rawdb.WriteDatabaseVersion(db, core.BlockChainVersion)
	}
	vmConfig := vm.Config{EnablePreimageRecording: cfg.EnablePreimageRecording}
	// The node journals the clean trie cache to <datadir>/triecache on stop
	// and every TrieCleanCacheRejournal (60 min); the benchmark does not.
	cacheConfig := &core.CacheConfig{
		TrieCleanLimit:      cfg.TrieCleanCache,
		TrieCleanRejournal:  cfg.TrieCleanCacheRejournal,
		TrieCleanNoPrefetch: cfg.NoPrefetch,
		TrieDirtyLimit:      cfg.TrieDirtyCache,
		TrieTimeLimit:       cfg.TrieTimeout,
		SnapshotLimit:       cfg.SnapshotCache,
		Preimages:           cfg.Preimages,
	}
	c, err := core.NewCore(db, &cfg.Miner, powConfig, &cfg.TxPool, &cfg.TxLookupLimit, chainConfig, cfg.SlicesRunning, 0, nil, engines, cacheConfig, vmConfig, cfg.Genesis, logger)
	if err != nil {
		return nil, err
	}
	return &contextNode{core: c, db: db}, nil
}

func cpuSeconds() (float64, float64) {
	var ru syscall.Rusage
	syscall.Getrusage(syscall.RUSAGE_SELF, &ru)
	tv := func(t syscall.Timeval) float64 { return float64(t.Sec) + float64(t.Usec)/1e6 }
	return tv(ru.Utime), tv(ru.Stime)
}

type replaySummary struct {
	Mode            string             `json:"mode"`
	PowCalls        uint64             `json:"pow_calls"`
	Gomaxprocs      int                `json:"gomaxprocs"`
	FirstZone       uint64             `json:"first_zone"`
	LastZone        uint64             `json:"last_zone"`
	Heights         uint64             `json:"heights"`
	Records         [3]uint64          `json:"records"`
	Workshares      uint64             `json:"workshares"`
	Transactions    uint64             `json:"transactions"`
	WallSeconds     float64            `json:"wall_seconds"`
	UserSeconds     float64            `json:"user_seconds"`
	SystemSeconds   float64            `json:"system_seconds"`
	TimingSeconds   map[string]float64 `json:"timing_seconds"`
	ZoneHead        string             `json:"zone_head"`
	ZoneHeadNumber  uint64             `json:"zone_head_number"`
	EVMRoot         string             `json:"evm_root"`
	UTXORoot        string             `json:"utxo_root"`
	EtxSetRoot      string             `json:"etx_set_root"`
	Failure         string             `json:"failure,omitempty"`
	FailureZone     uint64             `json:"failure_zone,omitempty"`
	ReferenceCommit string             `json:"reference_revision"`
}

func replay(args []string) {
	fs := flag.NewFlagSet("replay", flag.ExitOnError)
	exportDir := fs.String("export", "", "QEX1 export directory")
	datadir := fs.String("datadir", "", "new, empty data directory")
	paramsDir := fs.String("params", "", "go-quai params directory (genesis allocations, forfeiture addresses)")
	to := fs.Uint64("to", ^uint64(0), "last zone height")
	progressEvery := fs.Duration("progress", 30*time.Second, "time between progress lines")
	fs.Parse(args)
	if *exportDir == "" || *datadir == "" || *paramsDir == "" {
		fatalf("replay needs -export, -datadir and -params")
	}
	if entries, err := os.ReadDir(*datadir); err == nil && len(entries) > 0 {
		fatalf("%s is not empty", *datadir)
	}
	if err := os.MkdirAll(*datadir, 0o755); err != nil {
		fatalf("%v", err)
	}
	// The node runs in a directory with the VERSION file, which the miner
	// reads (params.readVersionFile); the benchmark runs in its data directory.
	version, err := os.ReadFile(filepath.Join(*paramsDir, "..", "VERSION"))
	if err != nil {
		fatalf("VERSION next to %s: %v", *paramsDir, err)
	}
	for _, p := range []*string{exportDir, datadir, paramsDir} {
		if *p, err = filepath.Abs(*p); err != nil {
			fatalf("%v", err)
		}
	}
	if err := os.WriteFile(filepath.Join(*datadir, "VERSION"), version, 0o644); err != nil {
		fatalf("%v", err)
	}
	if err := os.Chdir(*datadir); err != nil {
		fatalf("%v", err)
	}
	reader, err := openExport(*exportDir, 0, *to)
	if err != nil {
		fatalf("%v", err)
	}
	if common.HexToHash(reader.m.GenesisHash) != params.ProgpowColosseumGenesisHash {
		fatalf("export genesis %s is not the colosseum genesis", reader.m.GenesisHash)
	}
	started := time.Now()
	var nodes [3]*contextNode
	spent := &powTime{}
	for ctx := 0; ctx < 3; ctx++ {
		if nodes[ctx], err = newContext(ctx, *datadir, *paramsDir, spent); err != nil {
			fatalf("%s context: %v", contextNames[ctx], err)
		}
	}
	core.NativeBenchWire(nodes[0].core, nodes[1].core, nodes[2].core)

	s := replaySummary{Mode: "replay", Gomaxprocs: runtime.GOMAXPROCS(0), ReferenceCommit: referenceRevision, TimingSeconds: map[string]float64{}}
	timing := map[string]time.Duration{}
	fail := func(zone uint64, format string, args ...interface{}) {
		s.Failure = fmt.Sprintf(format, args...)
		s.FailureZone = zone
	}
	lastProgress := time.Now()
	first := true
	for s.Failure == "" {
		t := time.Now()
		h, err := reader.next()
		if err == io.EOF {
			break
		} else if err != nil {
			fail(s.LastZone+1, "export: %v", err)
			break
		}
		if h.zone == 0 {
			for ctx := 0; ctx < 3; ctx++ {
				if h.records[ctx] == nil || h.records[ctx].hash != params.ProgpowColosseumGenesisHash {
					fail(0, "export genesis record %d differs", ctx)
				}
			}
			continue
		}
		var wos [3]*types.WorkObject
		dom := -1
		for ctx := 0; ctx < 3 && s.Failure == ""; ctx++ {
			r := h.records[ctx]
			if r == nil {
				continue
			}
			if dom < 0 {
				dom = ctx
			}
			wo, err := decodeBlock(r.block, ctx)
			if err != nil {
				fail(h.zone, "decode %s record: %v", contextNames[ctx], err)
				break
			}
			if wo.Hash() != r.hash {
				fail(h.zone, "%s record hash %s, block hash %s", contextNames[ctx], r.hash.Hex(), wo.Hash().Hex())
				break
			}
			wos[ctx] = wo
		}
		timing["read_decode"] += time.Since(t)
		if s.Failure != "" {
			break
		}
		if first {
			s.FirstZone = h.zone
			first = false
		}
		t = time.Now()
		for ctx := dom; ctx < 3 && s.Failure == ""; ctx++ {
			if err := nodes[ctx].core.NativeBenchWriteBody(wos[ctx]); err != nil {
				fail(h.zone, "%s body: %v", contextNames[ctx], err)
			}
		}
		timing["write_body"] += time.Since(t)
		if s.Failure != "" {
			break
		}
		t = time.Now()
		if err := nodes[dom].core.NativeBenchInsert(wos[dom]); err != nil {
			fail(h.zone, "%s insert: %v", contextNames[dom], err)
			break
		}
		timing["append"] += time.Since(t)
		t = time.Now()
		for ctx := dom; ctx < 3 && s.Failure == ""; ctx++ {
			if err := nodes[ctx].core.NativeBenchSetHead(h.records[ctx].hash); err != nil {
				fail(h.zone, "%s head: %v", contextNames[ctx], err)
			}
		}
		key := "set_head"
		timing[key] += time.Since(t)
		if s.Failure != "" {
			break
		}
		if head := nodes[2].core.NativeBenchHead(); head.Hash() != h.records[2].hash {
			fail(h.zone, "zone head %s, expected %s", head.Hash().Hex(), h.records[2].hash.Hex())
			break
		}
		for ctx := dom; ctx < 3; ctx++ {
			s.Records[ctx]++
		}
		s.Workshares += uint64(len(wos[2].Uncles()))
		s.Transactions += uint64(len(wos[2].Transactions()))
		s.Heights++
		s.LastZone = h.zone
		if time.Since(lastProgress) >= *progressEvery {
			u, sy := cpuSeconds()
			fmt.Fprintf(os.Stderr, "zone %d: %d heights, %.1f blocks/s, cpu %.0f s\n", h.zone, s.Heights, float64(s.Heights)/time.Since(started).Seconds(), u+sy)
			lastProgress = time.Now()
		}
	}
	s.WallSeconds = time.Since(started).Seconds()
	s.UserSeconds, s.SystemSeconds = cpuSeconds()
	for k, v := range timing {
		s.TimingSeconds[k] = v.Seconds()
	}
	s.TimingSeconds["pow_calls"] = time.Duration(spent.nanos.Load()).Seconds()
	s.PowCalls = spent.calls.Load()
	if head := nodes[2].core.NativeBenchHead(); head != nil {
		s.ZoneHead = head.Hash().Hex()
		s.ZoneHeadNumber = head.NumberU64(common.ZONE_CTX)
		s.EVMRoot = head.EVMRoot().Hex()
		s.UTXORoot = head.UTXORoot().Hex()
		s.EtxSetRoot = head.EtxSetRoot().Hex()
	}
	out, _ := json.Marshal(s)
	fmt.Println(string(out))
	if s.Failure != "" {
		os.Exit(1)
	}
	// The process exits without stopping the cores: the measurement ends here,
	// and the Rust replay is measured to its last checkpoint.
	for ctx := 2; ctx >= 0; ctx-- {
		nodes[ctx].db.Close()
	}
}

// ---------------------------------------------------------------------------
// powbench

type powInput struct {
	zone    uint64
	index   int
	variant string
	height  uint64
	seal    common.Hash
	nonce   uint64
	claimed common.Hash
	diff    *big.Int
}

// verdict: 0 the computed mix is not the claimed mix, 1 the mix matches but
// the hash is above the block target 2^256/difficulty (verifySeal), 2 valid.
func verdict(in *powInput, mix, hash common.Hash) int {
	if mix != in.claimed {
		return 0
	}
	target := new(big.Int).Div(common.Big2e256, in.diff)
	if new(big.Int).SetBytes(hash.Bytes()).Cmp(target) > 0 {
		return 1
	}
	return 2
}

func powbench(args []string) {
	fs := flag.NewFlagSet("powbench", flag.ExitOnError)
	exportDir := fs.String("export", "", "QEX1 export directory")
	from := fs.Uint64("from", 1, "first zone height")
	to := fs.Uint64("to", 0, "last zone height")
	outPath := fs.String("out", "", "result lines (default: none)")
	variants := fs.Bool("variants", false, "add mutated inputs and later epochs")
	fs.Parse(args)
	reader, err := openExport(*exportDir, *from, *to)
	if err != nil {
		fatalf("%v", err)
	}
	var inputs []*powInput
	auxpow := 0
	epochLength := progpow.NativeBenchEpochLength()
	const maxProgpow = 1_301_100
	for {
		h, err := reader.next()
		if err == io.EOF {
			break
		} else if err != nil {
			fatalf("%v", err)
		}
		wo, err := decodeBlock(h.records[2].block, 2)
		if err != nil {
			fatalf("zone %d: %v", h.zone, err)
		}
		headers := append([]*types.WorkObjectHeader{wo.WorkObjectHeader()}, wo.Uncles()...)
		for i, header := range headers {
			if header.AuxPow() != nil {
				auxpow++
				continue
			}
			base := powInput{zone: h.zone, index: i, variant: "orig", height: header.PrimeTerminusNumber().Uint64(), seal: header.SealHash(), nonce: header.NonceU64(), claimed: header.MixHash(), diff: header.Difficulty()}
			in := base
			inputs = append(inputs, &in)
			if !*variants {
				continue
			}
			n := base
			n.variant, n.nonce = "nonce", base.nonce^1
			sl := base
			sl.variant = "seal"
			sl.seal[31] ^= 1
			m := base
			m.variant = "mix"
			m.claimed[0] ^= 1
			inputs = append(inputs, &n, &sl, &m)
			for k := uint64(1); k <= 3; k++ {
				e := base
				e.variant, e.height = fmt.Sprintf("e%d", k), base.height+k*epochLength
				if e.height <= maxProgpow {
					inputs = append(inputs, &e)
				}
			}
		}
	}
	epochs := map[uint64]*progpow.NativeBenchEpoch{}
	epochOf := func(number uint64) *progpow.NativeBenchEpoch {
		e := number / epochLength
		if epochs[e] == nil {
			epochs[e] = progpow.NativeBenchNewEpoch(number)
		}
		return epochs[e]
	}
	for _, in := range inputs {
		epochOf(in.height)
	}
	var out *bufio.Writer
	if *outPath != "" {
		f, err := os.Create(*outPath)
		if err != nil {
			fatalf("%v", err)
		}
		defer f.Close()
		out = bufio.NewWriter(f)
		defer out.Flush()
	}
	digest := sha256.New()
	var origTime time.Duration
	orig := 0
	verdicts := map[string][3]int{}
	// Markers for a driver that samples the process CPU time.
	fmt.Fprintln(os.Stderr, "powbench: light loop start")
	for _, in := range inputs {
		t := time.Now()
		mix, hash := epochOf(in.height).Light(in.seal, in.nonce, in.height)
		if in.variant == "orig" {
			origTime += time.Since(t)
			orig++
		}
		v := verdict(in, mix, hash)
		c := verdicts[in.variant]
		c[v]++
		verdicts[in.variant] = c
		line := fmt.Sprintf("%d %d %s %d %s %d %s %s %d\n", in.zone, in.index, in.variant, in.height, hex.EncodeToString(in.seal[:]), in.nonce, hex.EncodeToString(mix[:]), hex.EncodeToString(hash[:]), v)
		digest.Write([]byte(line))
		if out != nil {
			out.WriteString(line)
		}
	}
	fmt.Fprintln(os.Stderr, "powbench: light loop end")
	res := map[string]interface{}{
		"mode": "powbench", "from": *from, "to": *to, "inputs": len(inputs), "auxpow_headers": auxpow,
		"original": orig, "light_us_per_hash": float64(origTime.Microseconds()) / float64(max(orig, 1)),
		"verdicts": verdicts, "sha256": hex.EncodeToString(digest.Sum(nil)), "reference_revision": referenceRevision,
	}
	b, _ := json.Marshal(res)
	fmt.Println(string(b))
}
