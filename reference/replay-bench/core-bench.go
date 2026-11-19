// Offline benchmark adapter for the quai-rust replay comparison (ROADMAP
// Phase 4). `go build -overlay` adds this file to package core of the pinned
// go-quai v0.57.0 checkout; the checkout itself is not modified.
//
// The benchmark runs exported mainnet blocks through the path a go-quai node
// takes for a block it receives:
//   - Core.WriteBlock: the work-object binding and body checks, then the
//     candidate body (NativeBenchWriteBody);
//   - Core.InsertChain in the block's own context: Slice.Append with the
//     order, PCRC, HeaderChain.AppendHeader (VerifyHeader, with the
//     proof-of-work), the dominant contexts' Append of their subordinates and
//     the pending ETXs sent to the dominant context (NativeBenchInsert);
//   - the hierarchical coordinator's new head: Slice.GeneratePendingHeader
//     sets the head under headermu, which in the zone runs
//     StateProcessor.Apply (Process, ValidateState, the state commit)
//     (NativeBenchSetHead). The pending header that GeneratePendingHeader
//     then builds for miners is not built: the Rust replay builds none.
//
// The contexts talk to each other through nativeBenchBackend, which forwards
// each call to the Core as quai.QuaiAPIBackend does.
package core

import (
	"errors"
	"fmt"
	"math/big"

	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/trie"
)

type nativeBenchBackend struct{ c *Core }

var _ CoreBackend = nativeBenchBackend{}

func (b nativeBenchBackend) AddPendingEtxs(p types.PendingEtxs) error {
	return b.c.AddPendingEtxs(p)
}
func (b nativeBenchBackend) AddPendingEtxsRollup(p types.PendingEtxsRollup) error {
	return b.c.AddPendingEtxsRollup(p)
}
func (b nativeBenchBackend) RequestDomToAppendOrFetch(hash common.Hash, entropy *big.Int, order int) {
	b.c.RequestDomToAppendOrFetch(hash, entropy, order)
}
func (b nativeBenchBackend) Append(header *types.WorkObject, manifest types.BlockManifest, domTerminus common.Hash, domOrigin bool, newInboundEtxs types.Transactions) (types.Transactions, error) {
	return b.c.Append(header, manifest, domTerminus, domOrigin, newInboundEtxs)
}
func (b nativeBenchBackend) DownloadBlocksInManifest(hash common.Hash, manifest types.BlockManifest, entropy *big.Int) {
	b.c.DownloadBlocksInManifest(hash, manifest, entropy)
}
func (b nativeBenchBackend) GenerateRecoveryPendingHeader(pendingHeader *types.WorkObject, checkpointHashes types.Termini) error {
	return b.c.GenerateRecoveryPendingHeader(pendingHeader, checkpointHashes)
}
func (b nativeBenchBackend) GetPendingEtxsRollupFromSub(hash common.Hash, location common.Location) (types.PendingEtxsRollup, error) {
	return b.c.GetPendingEtxsRollupFromSub(hash, location)
}
func (b nativeBenchBackend) GetPendingEtxsFromSub(hash common.Hash, location common.Location) (types.PendingEtxs, error) {
	return b.c.GetPendingEtxsFromSub(hash, location)
}
func (b nativeBenchBackend) NewGenesisPendingHeader(pendingHeader *types.WorkObject, domTerminus common.Hash, hash common.Hash) error {
	return b.c.NewGenesisPendigHeader(pendingHeader, domTerminus, hash)
}
func (b nativeBenchBackend) GetManifest(blockHash common.Hash) (types.BlockManifest, error) {
	return b.c.GetManifest(blockHash)
}
func (b nativeBenchBackend) GetPrimeBlock(blockHash common.Hash) *types.WorkObject {
	return b.c.GetPrimeBlock(blockHash)
}
func (b nativeBenchBackend) GetKQuaiAndUpdateBit(blockHash common.Hash) (*big.Int, uint8, error) {
	return b.c.GetKQuaiAndUpdateBit(blockHash)
}
func (b nativeBenchBackend) ReceiveMinedHeader(header *types.WorkObject) error {
	_, err := b.c.ReceiveMinedHeader(header)
	return err
}

// NativeBenchWire connects prime, region 0 and zone 0-0 as
// cmd/utils.HierarchicalCoordinator.StartQuaiBackend does.
func NativeBenchWire(prime, region, zone *Core) {
	prime.SetSubInterface(nativeBenchBackend{region}, common.Location{0})
	region.SetDomInterface(nativeBenchBackend{prime})
	region.SetSubInterface(nativeBenchBackend{zone}, common.Location{0, 0})
	zone.SetDomInterface(nativeBenchBackend{region})
}

// NativeBenchWriteBody runs the checks of Core.WriteBlock and stores the
// candidate body; it does not queue the block (NativeBenchInsert appends it).
func (c *Core) NativeBenchWriteBody(block *types.WorkObject) error {
	if err := c.sl.hc.verifyWorkObjectBinding(block); err != nil {
		return fmt.Errorf("work object binding: %w", err)
	}
	if block.Location() == nil {
		return errors.New("block has no location")
	}
	if _, err := block.Body().ProtoEncode(types.BlockObject); err != nil {
		return fmt.Errorf("unserializable body: %w", err)
	}
	if c.sl.IsBlockHashABadHash(block.Hash()) {
		return errors.New("bad block hash")
	}
	if c.GetHeaderOrCandidateByHash(block.Hash()) == nil {
		c.sl.WriteBlock(block)
	}
	return nil
}

// NativeBenchInsert is Core.InsertChain for one block of this context's
// order, returning the append error instead of logging it.
func (c *Core) NativeBenchInsert(block *types.WorkObject) error {
	nodeCtx := c.NodeCtx()
	_, order, err := c.CalcOrder(block)
	if err != nil {
		return fmt.Errorf("order: %w", err)
	}
	if order != nodeCtx {
		return fmt.Errorf("block of order %d inserted in context %d", order, nodeCtx)
	}
	newPendingEtxs, err := c.sl.Append(block, common.Hash{}, false, nil)
	if err != nil {
		return fmt.Errorf("append: %w", err)
	}
	if nodeCtx > common.PRIME_CTX {
		pendingEtx := types.PendingEtxs{Header: block.ConvertToPEtxView(), OutboundEtxs: newPendingEtxs}
		if pendingEtx.IsValid(trie.NewStackTrie(nil)) {
			if err := c.SendPendingEtxsToDom(pendingEtx); err != nil {
				return fmt.Errorf("pending ETXs to dom: %w", err)
			}
		}
	}
	return nil
}

// NativeBenchSetHead sets the head of this context to the stored block
// `hash` under headermu, as Slice.GeneratePendingHeader does first. In the
// zone, HeaderChain.SetCurrentHeader appends the block's state.
func (c *Core) NativeBenchSetHead(hash common.Hash) error {
	block := c.sl.hc.GetBlockByHash(hash)
	if block == nil {
		return errors.New("block not found")
	}
	c.sl.hc.headermu.Lock()
	defer c.sl.hc.headermu.Unlock()
	return c.sl.hc.SetCurrentHeader(block)
}

// NativeBenchHead returns the current head of this context.
func (c *Core) NativeBenchHead() *types.WorkObject {
	return c.sl.hc.CurrentHeader()
}
