// Explicit offline/libp2p loopback oracle, with one enrolled Rust peer.
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	pb "github.com/dominant-strategies/go-quai/p2p/pb"
	"github.com/libp2p/go-libp2p"
	"github.com/libp2p/go-libp2p/core/network"
	"github.com/libp2p/go-libp2p/core/protocol"
	"github.com/libp2p/go-libp2p/p2p/muxer/yamux"
	"github.com/libp2p/go-libp2p/p2p/security/noise"
	"github.com/libp2p/go-libp2p/p2p/transport/tcp"
	"google.golang.org/protobuf/proto"
	"os"
	"sync/atomic"
)

var referenceRevision string

func main() {
	if len(os.Args) != 2 {
		panic("enrolled Rust peer ID required")
	}
	enrolled := os.Args[1]
	h, e := libp2p.New(libp2p.ListenAddrStrings("/ip4/127.0.0.1/tcp/0"), libp2p.NoTransports, libp2p.Transport(tcp.NewTCPTransport), libp2p.Security(noise.ID, noise.New), libp2p.Muxer(yamux.ID, yamux.DefaultTransport))
	if e != nil {
		panic(e)
	}
	defer h.Close()
	var count atomic.Uint64
	h.SetStreamHandler(protocol.ID("/quai/1.0.0"), func(s network.Stream) {
		defer s.Close()
		if s.Conn().RemotePeer().String() != enrolled {
			s.Reset()
			return
		}
		for {
			b, e := common.ReadMessageFromStream(s, protocol.ID("/quai/1.0.0"), nil)
			if e != nil {
				return
			}
			v := new(pb.QuaiMessage)
			if e = proto.Unmarshal(b, v); e != nil {
				s.Reset()
				return
			}
			request := v.GetRequest()
			if request == nil || request.GetHash() == nil {
				s.Reset()
				return
			}
			reply := &pb.QuaiMessage{Payload: &pb.QuaiMessage_Response{Response: &pb.QuaiResponseMessage{Id: request.Id, Location: request.Location, Response: &pb.QuaiResponseMessage_BlockHash{BlockHash: request.GetHash()}}}}
			b, e = proto.Marshal(reply)
			if e != nil {
				panic(e)
			}
			if e = common.WriteMessageToStream(s, b, protocol.ID("/quai/1.0.0"), nil); e != nil {
				return
			}
			count.Add(1)
		}
	})
	e = json.NewEncoder(os.Stdout).Encode(map[string]interface{}{"peer_id": h.ID().String(), "address": h.Addrs()[0].String(), "reference_revision": referenceRevision})
	if e != nil {
		panic(e)
	}
	bufio.NewReader(os.Stdin).ReadString('\n')
	fmt.Fprintf(os.Stderr, "loopback exchanges: %d\n", count.Load())
}
