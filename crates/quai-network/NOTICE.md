# Compatibility sources and attribution

The reference is Dominant Strategies go-quai revision
`33db82f2891b267ba898ca38bd2dd8bf75cbb072`:
https://github.com/dominant-strategies/go-quai.
This crate is GPL-3.0-only. The GPL version 3 license is retained in LICENSE.

Quai application framing, protocol IDs and topic construction follow pinned
common/stream_services.go, p2p/protocol/constants.go, p2p/pb/quai_messages.proto
and p2p/node/pubsubManager/utils.go. Request bookkeeping is a native bounded
implementation. The optional native-transport feature adds Rust libp2p
0.56.0 and the 0.4.0-alpha persistent-stream adapter, Noise/Yamux, identify,
Kademlia and manually validated GossipSub. Its TCP transport enforces IPv4
loopback before dialing or listening; peers require explicit enrollment.
This development library is excluded from the default shipped RPC binaries.
Early-block execution delivery and checkpoint restart tests do not qualify
full native sync or proof/hierarchy admission. Dependency licenses are retained
in THIRD_PARTY_LICENSES.txt.

Upstream execution/trie sources include Copyright 2014-2015 The go-ethereum
Authors and GNU LGPL version 3 or (at your option) any later version notices,
without warranty. The primitive attribution is included with the project and the standalone
bundle. Preserve those notices when distributing derived source or binaries.
