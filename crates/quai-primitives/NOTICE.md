# Compatibility source and attribution

Native work-header and transaction behavior follows Dominant Strategies go-quai
revision 33db82f2891b267ba898ca38bd2dd8bf75cbb072. Relevant files are
core/types/proto_block.proto, common/proto_common.proto, core/types/wo.go,
core/types/transaction.go, core/types/transaction_signing.go, core/types/utxo.go,
and common/address.go. Original project:
https://github.com/dominant-strategies/go-quai.
The project GPL version 3 license is retained in LICENSE; this crate is
GPL-3.0-only.

Transaction signature parsing and unsorted, untweaked MuSig2 key aggregation
match the pinned btcsuite btcec v2.3.2 implementation. The BSD license notice
for that source is preserved below.

Cryptographic operations use the pure Rust RustCrypto k256 0.13.4, SHA-2
0.10.9 and SHA-3 0.10.8 crates, under their MIT license option. Their notices
are preserved below. API references:
https://docs.rs/k256/0.13.4/k256/schnorr/struct.VerifyingKey.html
https://github.com/RustCrypto/elliptic-curves/tree/k256/v0.13.4/k256

Prost is Apache-2.0 licensed. BLAKE3 uses its pure Rust feature and is available
under upstream CC0-1.0 or Apache-2.0 terms. Preserve dependency notices when
distributing source or binaries.

Full raw schema declarations additionally follow core/rawdb/db.proto,
p2p/pb/quai_messages.proto and p2p/node/peerManager/peerdb/peer_info.proto.
Canonical body-header, list/trie, receipt/log and bloom behavior follows
core/types/block.go, hashing.go, receipt.go, log.go, bloom9.go and trie/.
The complete field declarations do not establish full semantic protocol parity.

The receipt/trie/gas source files retain the following upstream attribution:
Copyright 2014-2015 The go-ethereum Authors. Those source files are distributed
under the GNU Lesser General Public License, version 3 or (at your option) any
later version, without any warranty; see the original file notices and
https://www.gnu.org/licenses/lgpl-3.0.html. This derived crate is GPL-3.0-only;
the project GPL version 3 license remains included in LICENSE.

This crate provides bounded codecs, commitments and signature mathematics.
It does not validate complete blocks, UTXO ownership, balances, fees, fork
rules, auxiliary proof-of-work, chain selection, execution or synchronization.
Typed-RLP encoding is supported; typed-RLP decoding remains an open gate.

## btcsuite btcec v2.3.2 license

ISC License

Copyright (c) 2013-2022 The btcsuite developers
Copyright (c) 2015-2016 The Decred developers

Permission to use, copy, modify, and distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.

## RustCrypto MIT notices

Copyright (c) 2020-2024 RustCrypto Developers

Permission is hereby granted, free of charge, to any
person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the
Software without restriction, including without
limitation the rights to use, copy, modify, merge,
publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software
is furnished to do so, subject to the following
conditions:

The above copyright notice and this permission notice
shall be included in all copies or substantial portions
of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.

Copyright (c) 2006-2009 Graydon Hoare
Copyright (c) 2009-2013 Mozilla Foundation
Copyright (c) 2016 Artyom Pavlov

Permission is hereby granted, free of charge, to any
person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the
Software without restriction, including without
limitation the rights to use, copy, modify, merge,
publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software
is furnished to do so, subject to the following
conditions:

The above copyright notice and this permission notice
shall be included in all copies or substantial portions
of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.

Copyright (c) 2006-2009 Graydon Hoare
Copyright (c) 2009-2013 Mozilla Foundation
Copyright (c) 2014 Sébastien Martini
Copyright (c) 2016-2023 Artyom Pavlov, Marek Kotewicz

Permission is hereby granted, free of charge, to any
person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the
Software without restriction, including without
limitation the rights to use, copy, modify, merge,
publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software
is furnished to do so, subject to the following
conditions:

The above copyright notice and this permission notice
shall be included in all copies or substantial portions
of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.


Bounded auxiliary semantic codecs and template signing bytes follow pinned
core/types/auxpow.go and proto_block.proto. Auxiliary-aware header/work-object
decoding is explicit and remains separate from full consensus validity.
Ravencoin header serialization follows core/types/ravencoin.go, Copyright
2017-2025 The go-quai Authors, part of the go-quai library. Preserve those
notices and the project's GPL-3.0 license when distributing derived code.
