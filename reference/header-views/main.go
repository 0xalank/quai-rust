// Oracle: what go-quai v0.57.0 answers a peer's WorkObjectBlock and
// WorkObjectHeader requests for a block it holds (p2p/protocol/handler.go
// handleBlockRequest): the stored work object, decoded as the database
// decodes a block (types.BlockObject), then ConvertToBlockView or
// ConvertToHeaderView and ProtoEncode, as pb.EncodeQuaiResponse sends them.
//
// Input (argv[1]): a JSON list of {"context", "block"} with block the hex of
// proto.Marshal(wo.ProtoEncode(types.BlockObject)) in that context. Output:
// each input with "block_view" and "header_view" (hex), or "error".
package main

import (
	"encoding/hex"
	"encoding/json"
	"os"
	"strings"

	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core/types"
	"google.golang.org/protobuf/proto"
)

var referenceRevision string

type input struct {
	Name    string `json:"name"`
	Context int    `json:"context"`
	Block   string `json:"block"`
}

type output struct {
	Name       string `json:"name"`
	Context    int    `json:"context"`
	Block      string `json:"block"`
	BlockView  string `json:"block_view,omitempty"`
	HeaderView string `json:"header_view,omitempty"`
	Error      string `json:"error,omitempty"`
}

func main() {
	raw, err := os.ReadFile(os.Args[1])
	if err != nil {
		panic(err)
	}
	var inputs []input
	if err := json.Unmarshal(raw, &inputs); err != nil {
		panic(err)
	}
	// A zone node of slice [0 0] decodes and answers every context with its
	// own location.
	location := common.Location{0, 0}
	out := []output{}
	for _, in := range inputs {
		o := output{Name: in.Name, Context: in.Context, Block: in.Block}
		bytes, err := hex.DecodeString(strings.TrimPrefix(in.Block, "0x"))
		if err != nil {
			panic(err)
		}
		pb := &types.ProtoWorkObject{}
		if err := proto.Unmarshal(bytes, pb); err != nil {
			o.Error = "unmarshal: " + err.Error()
			out = append(out, o)
			continue
		}
		wo := &types.WorkObject{}
		if err := wo.ProtoDecode(pb, location, types.BlockObject); err != nil {
			o.Error = "decode: " + err.Error()
			out = append(out, o)
			continue
		}
		block, err := wo.ConvertToBlockView().ProtoEncode()
		if err != nil {
			o.Error = "block view: " + err.Error()
			out = append(out, o)
			continue
		}
		header, err := wo.ConvertToHeaderView().ProtoEncode()
		if err != nil {
			o.Error = "header view: " + err.Error()
			out = append(out, o)
			continue
		}
		b, err := proto.Marshal(block)
		if err != nil {
			panic(err)
		}
		h, err := proto.Marshal(header)
		if err != nil {
			panic(err)
		}
		o.BlockView = "0x" + hex.EncodeToString(b)
		o.HeaderView = "0x" + hex.EncodeToString(h)
		out = append(out, o)
	}
	enc := json.NewEncoder(os.Stdout)
	if err := enc.Encode(map[string]any{
		"reference_revision": referenceRevision,
		"cases":              out,
	}); err != nil {
		panic(err)
	}
}
