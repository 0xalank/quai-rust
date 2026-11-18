// Linked prime blocks through the official prime branch of Slice.Append.
// go-quai is GPL-3.0; see this folder's NOTICE.md and LICENSE.
package core

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"math/big"
	"regexp"
	"sort"

	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus/misc"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/ethdb"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"github.com/dominant-strategies/go-quai/trie"
	lru "github.com/hashicorp/golang-lru/v2"
	"google.golang.org/protobuf/proto"
)

func ptWire(w *types.WorkObject) string {
	p, e := w.ProtoEncode(types.BlockObject)
	if e != nil {
		panic(e)
	}
	b, e := proto.Marshal(p)
	if e != nil {
		panic(e)
	}
	return "0x" + hex.EncodeToString(b)
}

func ptTx(t *types.Transaction) string {
	p, e := t.ProtoEncode()
	if e != nil {
		panic(e)
	}
	b, e := proto.Marshal(p)
	if e != nil {
		panic(e)
	}
	return "0x" + hex.EncodeToString(b)
}

func ptTxs(txs types.Transactions) []string {
	out := []string{}
	for _, tx := range txs {
		out = append(out, ptTx(tx))
	}
	return out
}

func ptClone(txs types.Transactions) types.Transactions {
	out := types.Transactions{}
	for _, tx := range txs {
		out = append(out, types.NewTx(tx.Inner()))
	}
	return out
}

// Bind the body header and, after KawPow, a donor AuxPow to the work header.
// This builds component inputs; proof work and signatures are not asserted.
func ptBind(w *types.WorkObject) {
	h := w.WorkObjectHeader()
	h.SetHeaderHash(w.Header().Hash())
	if w.PrimeTerminusNumber().Uint64() < params.KawPowForkBlock {
		return
	}
	if h.KawpowDifficulty() == nil {
		z := func() *big.Int { return new(big.Int) }
		h.SetKawpowDifficulty(z())
		h.SetShaShareTarget(z())
		h.SetScryptShareTarget(z())
		h.SetShaDiffAndCount(types.NewPowShareDiffAndCount(z(), z(), z()))
		h.SetScryptDiffAndCount(types.NewPowShareDiffAndCount(z(), z(), z()))
	}
	t := types.DefaultKawpowAuxTemplate()
	tx := types.NewAuxPowCoinbaseTx(types.Kawpow, t.Height(), t.CoinbaseOut(), w.SealHash(), t.SignatureTime())
	mr := types.CalculateMerkleRoot(types.Kawpow, tx, t.MerkleBranch())
	donor := types.NewBlockHeader(types.Kawpow, int32(t.Version()), t.PrevHash(), mr, t.SignatureTime()+1, t.Bits(), 0, t.Height())
	h.SetAuxPow(types.NewAuxPow(types.Kawpow, donor, t.AuxPow2(), t.Sigs(), t.MerkleBranch(), tx))
}

// NativePrimeTransitionAppendBranch is the prime branch of Slice.Append,
// copied byte-identical by the driver from core/slice.go.
func NativePrimeTransitionAppendBranch(sl *Slice, header, block *types.WorkObject, newInboundEtxs types.Transactions, pending *ethdb.Batch) (types.Transactions, error) {
	nodeCtx := sl.NodeCtx()
	batch := sl.sliceDb.NewBatch()
@@APPEND_BRANCH@@
	*pending = batch
	return newInboundEtxs, nil
}

func ptDigest(set types.TokenChoiceSet) string {
	h := sha256.New()
	var row [48]byte
	for _, c := range set {
		binary.BigEndian.PutUint64(row[:8], c.Quai)
		binary.BigEndian.PutUint64(row[8:16], c.Qi)
		c.Diff.FillBytes(row[16:])
		h.Write(row[:])
	}
	return "0x" + hex.EncodeToString(h.Sum(nil))
}

func ptSum(set types.TokenChoiceSet) string {
	n := new(big.Int)
	for _, c := range set {
		n.Add(n, c.Diff)
	}
	return n.String()
}

// One seeded token choice row per prime block number; the Rust test builds
// the same rows from (base, step).
func ptRow(n uint64, base, step *big.Int) types.TokenChoices {
	diff := new(big.Int).Mul(step, new(big.Int).SetUint64(n%997))
	return types.TokenChoices{Quai: n % 13, Qi: n % 11, Diff: diff.Add(diff, base)}
}

// The set Append would have stored for prime block x: the rows of the blocks
// ControllerKickInBlock+1..x while it fills, the last TokenChoiceSetSize after.
func ptSet(x uint64, base, step *big.Int) (types.TokenChoiceSet, uint64, uint64) {
	set := types.NewTokenChoiceSet()
	k := params.ControllerKickInBlock
	if x <= k {
		return set, 0, 0
	}
	first, count := k+1, x-k
	if count > params.TokenChoiceSetSize {
		first, count = x-params.TokenChoiceSetSize+1, params.TokenChoiceSetSize
	}
	for i := uint64(0); i < count; i++ {
		set[i] = ptRow(first+i, base, step)
	}
	return set, first, count
}

type ptSegment struct {
	name   string
	start  uint64 // first validated prime block
	blocks int
	// Rates of the canonical blocks start-1-4000 and later: the starting
	// rate, or alternating around the parent rate.
	varied bool
	// Seed values of the parent and grandparent.
	rate, discount, flow *big.Int
	// Block difficulty around which the chain moves.
	difficulty int64
}

type ptChain struct {
	db      ethdb.Database
	hc      *HeaderChain
	sl      *Slice
	segment ptSegment
	index   int
}

// Store a block the way GetBlock reads it (termini, number, work object):
// a segment holds more blocks than the block cache keeps.
func (c *ptChain) cache(w *types.WorkObject) {
	rawdb.WriteTermini(c.db, w.Hash(), types.EmptyTermini())
	rawdb.WriteWorkObject(c.db, w.Hash(), w, types.BlockObject, common.PRIME_CTX)
	c.hc.bc.blockCache.Add(w.Hash(), *w)
}

func (c *ptChain) block(n uint64, parent common.Hash, difficulty, miner *big.Int) *types.WorkObject {
	w := types.EmptyWorkObject(common.ZONE_CTX)
	zone := n * 46 / 10
	w.SetNumber(new(big.Int).SetUint64(n), common.PRIME_CTX)
	w.SetNumber(new(big.Int).SetUint64(n*23/10), common.REGION_CTX)
	w.SetNumber(new(big.Int).SetUint64(zone), common.ZONE_CTX)
	w.WorkObjectHeader().SetLocation(common.Location{0, 0})
	w.WorkObjectHeader().SetPrimaryCoinbase(common.ZeroAddress(common.Location{0, 0}))
	w.WorkObjectHeader().SetPrimeTerminusNumber(new(big.Int).SetUint64(n - 1))
	w.WorkObjectHeader().SetDifficulty(new(big.Int).Set(difficulty))
	w.WorkObjectHeader().SetTime(1700000000 + zone*5)
	w.Header().SetParentHash(parent, common.PRIME_CTX)
	w.Header().SetParentHash(parent, common.REGION_CTX)
	w.SetParentHash(parent, common.ZONE_CTX)
	w.Header().SetMinerDifficulty(new(big.Int).Set(miner))
	w.Header().SetExchangeRate(new(big.Int).Set(params.ExchangeRate))
	w.Header().SetConversionFlowAmount(new(big.Int).Set(params.StartingConversionFlowAmount))
	w.Header().SetKQuaiDiscount(new(big.Int).Set(params.StartingKQuaiDiscount))
	w.Header().SetAvgTxFees(big.NewInt(int64(n%1000) * 1000000007))
	ptBind(w)
	return w
}

func (c *ptChain) difficulty(n uint64) *big.Int {
	d := big.NewInt(c.segment.difficulty)
	return d.Add(d, new(big.Int).SetUint64((n*7919)%100003*1000003))
}

func (c *ptChain) oldRate(n uint64, around *big.Int) *big.Int {
	if !c.segment.varied {
		return new(big.Int).Set(params.ExchangeRate)
	}
	r := new(big.Int).Set(around)
	delta := new(big.Int).Div(around, big.NewInt(int64(50+n%37)))
	if n%2 == 0 {
		return r.Add(r, delta)
	}
	return r.Sub(r, delta)
}

// The canonical block at prime number n with exchange rate rate.
func (c *ptChain) canonical(n uint64, rate *big.Int) {
	w := c.block(n, common.Hash{30: byte(n >> 8), 31: byte(n)}, c.difficulty(n), c.difficulty(n))
	w.Header().SetExchangeRate(new(big.Int).Set(rate))
	ptBind(w)
	c.cache(w)
	rawdb.WriteCanonicalHash(c.db, w.Hash(), n)
}

var ptQuai = common.BytesToAddress([]byte{0x00, 0x11, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 1}, common.Location{0, 0})
var ptQi = common.BytesToAddress([]byte{0x00, 0x91, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 2}, common.Location{0, 0})

// Newly confirmed ETXs of prime block n: coinbase ETXs to both ledgers and,
// after the kick-in, conversions with every slip shape and default ETXs.
func (c *ptChain) inbound(n uint64, ref *types.WorkObject) types.Transactions {
	txs := types.Transactions{}
	count := 6 + int(n%5)
	if n > params.ControllerKickInBlock {
		count = 14 + int(n%11)
	}
	for j := 0; j < count; j++ {
		seed := int(n%97) + j*31 + c.index*7
		recipient := ptQuai
		if (seed/3)%2 == 0 {
			recipient = ptQi
		}
		kind := uint64(types.CoinbaseType)
		value := big.NewInt(int64(1000000000000000000 + seed*997))
		data := []byte{byte(seed % 4)}
		data = append(data, make([]byte, 32)...)
		if n > params.ControllerKickInBlock && j%5 != 0 {
			kind = types.ConversionType
			if j%11 == 3 {
				kind = types.DefaultType
			}
			target := new(big.Int).Mul(ref.ConversionFlowAmount(), big.NewInt(int64(seed%9+1)))
			target.Div(target, big.NewInt(int64(23+seed%41)))
			if seed%13 == 0 {
				target.Mul(target, big.NewInt(40))
			}
			value = new(big.Int).Set(target)
			if recipient.IsInQuaiLedgerScope() {
				value = misc.QuaiToQi(ref, ref.ExchangeRate(), ref.MinerDifficulty(), target)
			}
			slip := []uint16{0, 1, 29, 30, 31, 500, 2500, 5000, 8999, 9000, 9001, 65535}[seed%12]
			data = []byte{byte(slip >> 8), byte(slip)}
			if seed%17 == 0 {
				data = []byte{7}
			}
			if seed%19 == 0 {
				data = []byte{}
			}
			if seed%23 == 0 {
				data = append(data, 1, 2, 3)
			}
		}
		if value.Sign() == 0 {
			value.SetUint64(1)
		}
		txs = append(txs, types.NewTx(&types.ExternalTx{
			OriginatingTxHash: common.Hash{20: byte(c.index), 28: byte(n >> 16), 29: byte(n >> 8), 30: byte(n), 31: byte(j + 1)},
			ETXIndex:          uint16(j),
			Gas:               100000,
			To:                &recipient,
			Sender:            ptQuai,
			Value:             value,
			Data:              data,
			AccessList:        types.AccessList{},
			EtxType:           kind,
		}))
	}
	return txs
}

var ptLocal = regexp.MustCompile(`invalid (conversion flow amount|exchange rate|newKQuaiDiscount) used \(remote: ([0-9]+) local: ([0-9]+)\)`)

// Run the official branch on block w with its inbound list; the batch is
// written only when commit is set.
func (c *ptChain) run(w *types.WorkObject, raw types.Transactions, commit bool) (types.Transactions, error) {
	var pending ethdb.Batch
	out, err := NativePrimeTransitionAppendBranch(c.sl, w, w, ptClone(raw), &pending)
	if err == nil && commit {
		if e := pending.Write(); e != nil {
			panic(e)
		}
	}
	return out, err
}

// The header rule of core/headerchain_validation.go for prime blocks.
func (c *ptChain) minerRule(parent, w *types.WorkObject) error {
	expected := c.hc.ComputeMinerDifficulty(parent)
	if w.MinerDifficulty().Cmp(expected) != 0 {
		return fmt.Errorf("invalid miner difficulty: have %v, want %v", w.MinerDifficulty(), expected)
	}
	return nil
}

// Fields for block w from the unmodified verifier: the starting values up to
// the kick-in, else the values verifyParentExchangeRateAndFlowAmount names.
func (c *ptChain) fill(w *types.WorkObject) {
	if w.NumberU64(common.PRIME_CTX) <= params.ControllerKickInBlock {
		return
	}
	w.Header().SetExchangeRate(new(big.Int))
	w.Header().SetConversionFlowAmount(new(big.Int))
	w.Header().SetKQuaiDiscount(new(big.Int))
	for attempt := 0; attempt < 4; attempt++ {
		ptBind(w)
		err := c.sl.verifyParentExchangeRateAndFlowAmount(w)
		if err == nil {
			return
		}
		m := ptLocal.FindStringSubmatch(err.Error())
		if m == nil {
			panic(err)
		}
		v, _ := new(big.Int).SetString(m[3], 10)
		switch m[1] {
		case "conversion flow amount":
			w.Header().SetConversionFlowAmount(v)
		case "exchange rate":
			w.Header().SetExchangeRate(v)
		default:
			w.Header().SetKQuaiDiscount(v)
		}
	}
	panic("verifier did not converge")
}

func ptVerdict(err error) interface{} {
	if err == nil {
		return nil
	}
	return err.Error()
}

func NativePrimeTransitionVectors() map[string]interface{} {
	k := params.ControllerKickInBlock
	exchange := new(big.Int).Set(params.ExchangeRate)
	flow := new(big.Int).Set(params.StartingConversionFlowAmount)
	segments := []ptSegment{
		{"kick-in", k - 2, 12, false, exchange, params.StartingKQuaiDiscount, flow, 300000000000000},
		{"first feedback", k + params.TokenChoiceSetSize - 2, 9, false, exchange, big.NewInt(37), new(big.Int).Mul(flow, big.NewInt(3)), 310000000000000},
		{"conversion slip change", params.ConversionSlipChangeBlock - 2, 7, true, new(big.Int).Mul(exchange, big.NewInt(3)), big.NewInt(1234), new(big.Int).Div(flow, big.NewInt(4)), 320000000000000},
		{"kquai change", params.KQuaiChangeBlock - 2, 8, true, new(big.Int).Mul(exchange, big.NewInt(5)), big.NewInt(800), new(big.Int).Mul(flow, big.NewInt(2)), 400000000000000},
		{"kquai change hold end", params.KQuaiChangeBlock + params.KQuaiChangeHoldInterval - 3, 8, true, new(big.Int).Div(new(big.Int).Mul(exchange, big.NewInt(11)), big.NewInt(10)), big.NewInt(2500), new(big.Int).Mul(flow, big.NewInt(7)), 410000000000000},
		{"second table row", params.KQuaiChangeTable[1][0] - 2, 7, true, new(big.Int).Mul(exchange, big.NewInt(2)), big.NewInt(90), flow, 500000000000000},
		{"second row hold end", params.KQuaiChangeTable[1][0] + params.KQuaiChangeHoldInterval - 3, 8, true, new(big.Int).Div(new(big.Int).Mul(exchange, big.NewInt(3)), big.NewInt(2)), big.NewInt(4100), new(big.Int).Mul(flow, big.NewInt(5)), 520000000000000},
		{"kawpow reset", params.KawPowForkBlock - 2, 7, true, new(big.Int).Mul(exchange, big.NewInt(4)), big.NewInt(300), new(big.Int).Mul(flow, big.NewInt(3)), 900000000000000},
		{"kquai freeze", params.KQuaiFreezeForkBlock - 2, 7, true, new(big.Int).Mul(exchange, big.NewInt(70)), big.NewInt(150), new(big.Int).Mul(flow, big.NewInt(9)), 950000000000000},
	}
	out := []interface{}{}
	accepted, rejectedFields, rejectedHistory, acceptedHistory, outputs := 0, 0, 0, 0, 0
	for index, segment := range segments {
		db := rawdb.NewMemoryDatabase(log.Global)
		cfg := *params.TestChainConfig
		cfg.Location = common.Location{}
		numbers, _ := lru.New[common.Hash, uint64](4096)
		hc := &HeaderChain{config: &cfg, headerDb: db, bc: NewTestBodyDb(db), numberCache: numbers, logger: log.Global}
		hc.bc.chainConfig = &cfg
		// StateProcessor.GetKQuaiAndUpdateBit, as Slice.GetKQuaiAndUpdateBit
		// reaches it for every zone parent.
		hc.fetchKQuaiAndUpdateBit = func(hash common.Hash) (*big.Int, uint8, error) {
			return (&StateProcessor{}).GetKQuaiAndUpdateBit(nil)
		}
		sl := &Slice{hc: hc, sliceDb: db, config: &cfg, logger: log.Global}
		c := &ptChain{db: db, hc: hc, sl: sl, segment: segment, index: index}
		base := big.NewInt(segment.difficulty)
		step := big.NewInt(1000003 + int64(index)*7)

		// Seed: grandparent and parent, their stored sets, the parent's
		// inbound ETXs and the canonical rates the segment reads.
		gn, pn := segment.start-2, segment.start-1
		grand := c.block(gn, common.Hash{31: 1}, c.difficulty(gn), c.difficulty(gn))
		grand.Header().SetExchangeRate(new(big.Int).Set(segment.rate))
		grand.Header().SetKQuaiDiscount(new(big.Int).Set(segment.discount))
		grand.Header().SetConversionFlowAmount(new(big.Int).Set(segment.flow))
		ptBind(grand)
		parent := c.block(pn, grand.Hash(), c.difficulty(pn), hc.ComputeMinerDifficulty(grand))
		parent.Header().SetExchangeRate(new(big.Int).Set(segment.rate))
		parent.Header().SetKQuaiDiscount(new(big.Int).Set(segment.discount))
		parent.Header().SetConversionFlowAmount(new(big.Int).Set(segment.flow))
		if pn <= k {
			parent.Header().SetExchangeRate(new(big.Int).Set(params.ExchangeRate))
			parent.Header().SetKQuaiDiscount(new(big.Int).Set(params.StartingKQuaiDiscount))
			parent.Header().SetConversionFlowAmount(new(big.Int).Set(params.StartingConversionFlowAmount))
		}
		ptBind(parent)
		c.cache(grand)
		c.cache(parent)
		grandSet, grandFirst, grandCount := ptSet(gn, base, step)
		parentSet, parentFirst, parentCount := ptSet(pn, base, step)
		if grandCount > 0 {
			if e := rawdb.WriteTokenChoicesSet(db, grand.Hash(), &grandSet); e != nil {
				panic(e)
			}
		}
		if parentCount > 0 {
			if e := rawdb.WriteTokenChoicesSet(db, parent.Hash(), &parentSet); e != nil {
				panic(e)
			}
		}
		parentInbound := c.inbound(pn, grand)
		rawdb.WriteInboundEtxs(db, parent.Hash(), parentInbound)
		rates := []interface{}{}
		for n := pn - params.MinerDifficultyWindow; n < segment.start+uint64(segment.blocks)-params.MinerDifficultyWindow; n++ {
			r := c.oldRate(n, segment.rate)
			c.canonical(n, r)
			rates = append(rates, []interface{}{n, r.String()})
		}
		seed := map[string]interface{}{
			"grandparent": ptWire(grand), "grandparent_hash": grand.Hash().Hex(),
			"parent": ptWire(parent), "parent_hash": parent.Hash().Hex(),
			"parent_inbound": ptTxs(parentInbound),
			"rates":          rates,
			"row_base":       base.String(), "row_step": step.String(),
			"grandparent_window": map[string]interface{}{"first": grandFirst, "count": grandCount, "sha256": ptDigest(grandSet), "difficulty_sum": ptSum(grandSet)},
			"parent_window":      map[string]interface{}{"first": parentFirst, "count": parentCount, "sha256": ptDigest(parentSet), "difficulty_sum": ptSum(parentSet)},
		}

		blocks := []interface{}{}
		for i := 0; i < segment.blocks; i++ {
			n := segment.start + uint64(i)
			w := c.block(n, parent.Hash(), c.difficulty(n), hc.ComputeMinerDifficulty(parent))
			c.fill(w)
			ptBind(w)
			raw := c.inbound(n, parent)
			row := map[string]interface{}{"number": n, "block": ptWire(w), "hash": w.Hash().Hex(), "inbound": ptTxs(raw)}

			// Changed header fields: Go refuses each.
			mutations := []interface{}{}
			type field struct {
				name string
				get  func(*types.WorkObject) *big.Int
				set  func(*types.WorkObject, *big.Int)
			}
			fields := []field{
				{"exchange_rate", func(x *types.WorkObject) *big.Int { return x.ExchangeRate() }, func(x *types.WorkObject, v *big.Int) { x.Header().SetExchangeRate(v) }},
				{"k_quai_discount", func(x *types.WorkObject) *big.Int { return x.KQuaiDiscount() }, func(x *types.WorkObject, v *big.Int) { x.Header().SetKQuaiDiscount(v) }},
				{"conversion_flow_amount", func(x *types.WorkObject) *big.Int { return x.ConversionFlowAmount() }, func(x *types.WorkObject, v *big.Int) { x.Header().SetConversionFlowAmount(v) }},
				{"miner_difficulty", func(x *types.WorkObject) *big.Int { return x.MinerDifficulty() }, func(x *types.WorkObject, v *big.Int) { x.Header().SetMinerDifficulty(v) }},
			}
			for _, f := range fields {
				for _, delta := range []int64{1, -1} {
					v := new(big.Int).Add(f.get(w), big.NewInt(delta))
					if v.Sign() < 0 {
						continue
					}
					m := types.CopyWorkObject(w)
					f.set(m, v)
					ptBind(m)
					var err error
					if f.name == "miner_difficulty" {
						err = c.minerRule(parent, m)
					} else {
						_, err = c.run(m, raw, false)
					}
					if err == nil {
						panic(fmt.Sprintf("%s %d: changed %s accepted", segment.name, n, f.name))
					}
					rejectedFields++
					mutations = append(mutations, map[string]interface{}{"field": f.name, "value": v.String(), "block": ptWire(m), "go_error": err.Error()})
				}
			}
			row["mutations"] = mutations

			// Changed history the verifier reads for this block.
			history := []interface{}{}
			if n > k {
				// Drop the parent's largest conversion from its stored inbound ETXs.
				stored := rawdb.ReadInboundEtxs(db, parent.Hash())
				changed := types.Transactions{}
				dropped := -1
				for j, tx := range stored {
					if dropped < 0 && types.IsConversionTx(tx) {
						dropped = j
						continue
					}
					changed = append(changed, tx)
				}
				if dropped < 0 && len(stored) > 0 {
					changed = stored[1:]
				}
				rawdb.WriteInboundEtxs(db, parent.Hash(), changed)
				_, err := c.run(w, raw, false)
				rawdb.WriteInboundEtxs(db, parent.Hash(), stored)
				history = append(history, map[string]interface{}{"kind": "parent_inbound", "inbound": ptTxs(changed), "go_error": ptVerdict(err)})
				if err == nil {
					acceptedHistory++
				} else {
					rejectedHistory++
				}
				// Flip the parent's rate comparison WindowSize blocks back.
				old := parent.NumberU64(common.PRIME_CTX) - params.MinerDifficultyWindow
				oldBlock := hc.GetBlockByNumber(old)
				flipped := new(big.Int).Sub(parent.ExchangeRate(), big.NewInt(1))
				if parent.ExchangeRate().Cmp(oldBlock.ExchangeRate()) > 0 {
					flipped = new(big.Int).Set(parent.ExchangeRate())
				}
				c.canonical(old, flipped)
				_, err = c.run(w, raw, false)
				c.cache(oldBlock)
				rawdb.WriteCanonicalHash(db, oldBlock.Hash(), old)
				history = append(history, map[string]interface{}{"kind": "old_rate", "number": old, "rate": flipped.String(), "go_error": ptVerdict(err)})
				if err == nil {
					acceptedHistory++
				} else {
					rejectedHistory++
				}
				// Raise one row of the grandparent's stored token choice set.
				if gs := rawdb.ReadTokenChoicesSet(db, grand.Hash()); gs != nil && pn > k+1 {
					changedSet := *gs
					at := (n * 31) % params.TokenChoiceSetSize
					changedSet[at] = types.TokenChoices{Quai: gs[at].Quai, Qi: gs[at].Qi, Diff: new(big.Int).Add(gs[at].Diff, new(big.Int).Mul(base, big.NewInt(1000)))}
					if e := rawdb.WriteTokenChoicesSet(db, grand.Hash(), &changedSet); e != nil {
						panic(e)
					}
					_, err = c.run(w, raw, false)
					if e := rawdb.WriteTokenChoicesSet(db, grand.Hash(), gs); e != nil {
						panic(e)
					}
					history = append(history, map[string]interface{}{"kind": "grandparent_window", "index": at, "difficulty": changedSet[at].Diff.String(), "go_error": ptVerdict(err)})
					if err == nil {
						acceptedHistory++
					} else {
						rejectedHistory++
					}
				}
				// Raise one row of the parent's stored set: Append prices this
				// block's conversions at a different forward rate.
				if ps := rawdb.ReadTokenChoicesSet(db, parent.Hash()); ps != nil && n > k+1 {
					changedSet := *ps
					at := (n * 17) % params.TokenChoiceSetSize
					changedSet[at] = types.TokenChoices{Quai: ps[at].Quai, Qi: ps[at].Qi, Diff: new(big.Int).Add(ps[at].Diff, new(big.Int).Mul(base, big.NewInt(4000)))}
					if e := rawdb.WriteTokenChoicesSet(db, parent.Hash(), &changedSet); e != nil {
						panic(e)
					}
					priced, err := c.run(w, raw, false)
					if e := rawdb.WriteTokenChoicesSet(db, parent.Hash(), ps); e != nil {
						panic(e)
					}
					if err != nil {
						panic(err)
					}
					history = append(history, map[string]interface{}{"kind": "parent_window", "index": at, "difficulty": changedSet[at].Diff.String(), "go_error": nil, "output": ptTxs(priced)})
					acceptedHistory++
				}
			}
			row["history"] = history

			// The block itself: header rule, then the branch, committed.
			if err := c.minerRule(parent, w); err != nil {
				panic(err)
			}
			priced, err := c.run(w, raw, true)
			if err != nil {
				panic(fmt.Sprintf("%s %d: %v", segment.name, n, err))
			}
			accepted++
			outputs += len(priced)
			row["output"] = ptTxs(priced)
			row["output_root"] = types.DeriveSha(priced, trie.NewStackTrie(nil)).Hex()
			reverts := 0
			for _, tx := range priced {
				if tx.EtxType() == types.ConversionRevertType {
					reverts++
				}
			}
			row["refunds"] = reverts
			row["fields"] = map[string]interface{}{"exchange_rate": w.ExchangeRate().String(), "k_quai_discount": w.KQuaiDiscount().String(), "conversion_flow_amount": w.ConversionFlowAmount().String(), "miner_difficulty": w.MinerDifficulty().String()}
			if n > k {
				set := rawdb.ReadTokenChoicesSet(db, w.Hash())
				if set == nil {
					panic("stored token choice set")
				}
				row["window_sha256"] = ptDigest(*set)
				row["difficulty_sum"] = ptSum(*set)
				// The update bit is always 1, so Append prices with the
				// controller rate of the parent and this block's set.
				forward, err := CalculateBetaFromMiningChoiceAndConversions(hc, parent, w.ExchangeRate(), *set)
				if err != nil {
					panic(err)
				}
				row["forward_rate"] = forward.String()
			}
			// Append stores the raw list for the next verifier.
			rawdb.WriteInboundEtxs(db, w.Hash(), raw)
			c.cache(w)
			blocks = append(blocks, row)
			grand, parent = parent, w
		}
		out = append(out, map[string]interface{}{"name": segment.name, "seed": seed, "blocks": blocks})
		db.Close()
	}
	return map[string]interface{}{
		"segments":                  out,
		"accepted_blocks":           accepted,
		"go_rejected_field_changes": rejectedFields,
		"go_rejected_history":       rejectedHistory,
		"go_accepted_history":       acceptedHistory,
		"priced_etxs":               outputs,
	}
}
