# Compatibility sources and attribution

The reference is Dominant Strategies go-quai revision
`33db82f2891b267ba898ca38bd2dd8bf75cbb072`:
https://github.com/dominant-strategies/go-quai.
This crate is GPL-3.0-only. The GPL version 3 license is retained in LICENSE.

Intrinsic transaction gas follows pinned core/state_transition.go and
core/types/qi_tx.go with params/protocol_params.go. Qi input-helper behavior
follows ValidateQiTxInputs in core/state_processor.go, core/types/utxo.go and
crypto/crypto.go. That helper excludes signatures, complete output/value/fee
policy and full transaction validity. Duplicate-spend checking is a separate
native invariant. ETX push/pop/index/root behavior follows the live
core/state/statedb.go SecureTrie path; core/types/etx_set.go is a different
legacy representation and is not used here. Auxiliary ETX-trie metadata is
preserved opaquely. The native stateful increment adds nested journaled calls,
creation, nine precompiles, signed Quai transaction settlement, Qi UTXO/fee
settlement and outbound ETX emission. Incoming ETX/lockup/allocation/reward
replay, full transaction domains and consensus admission remain open.

Upstream execution/trie sources include Copyright 2014-2015 The go-ethereum
Authors and GNU LGPL version 3 or (at your option) any later version notices,
without warranty. The primitive attribution is included with the project and the standalone
bundle. Preserve those notices when distributing derived source or binaries.

The bounded interpreter/account roots follow pinned core/vm, core/state and
params rules. Early private replay covers three linked blocks through the
actual reference StateProcessor.Process. It is not complete block validity.
State-gas logarithm normalization is adapted
from modernc.org/mathutil v1.6.0 (binarylog.go and sqr.go), licensed as follows:

Copyright (c) 2014 The mathutil Authors. All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are
met:

   * Redistributions of source code must retain the above copyright
notice, this list of conditions and the following disclaimer.
   * Redistributions in binary form must reproduce the above
copyright notice, this list of conditions and the following disclaimer
in the documentation and/or other materials provided with the
distribution.
   * Neither the names of the authors nor the names of the
contributors may be used to endorse or promote products derived from
this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

Fixed-width arithmetic uses RustCrypto crypto-bigint v0.5.5 under its MIT
license option. Copyright and permission notice:

Copyright (c) 2021 The RustCrypto Project Developers

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

MuHash3072 follows github.com/kaspanet/go-muhash v0.0.4 and the Quai
crypto/multiset wrapper. Preserve the upstream ISC terms:

Copyright (c) 2018-2019 The kaspanet developers
Copyright (c) 2013-2018 The btcsuite developers
Copyright (c) 2015-2016 The Decred developers
Copyright (c) 2013-2014 Conformal Systems LLC.

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

Additional precompile, RLP, UTXO and MuHash behavior follows the pinned
core/vm/{evm,contracts,gas_table,instructions}.go, core/state_processor.go,
core/types/utxo.go and crypto/multiset sources. Rust dependency license texts
are retained in the project and standalone THIRD_PARTY_LICENSES.txt.

The native allocation index in assets/allocations.bin is derived from the
official v0.57.0 go-quai parameter files at revision
33db82f2891b267ba898ca38bd2dd8bf75cbb072, copyright Dominant Strategies Inc.
The GPL-3.0-only terms apply. assets/allocations.json records the source hashes.
The explicit offline import script is scripts/gen_allocation_asset.py; ordinary
Rust builds consume the committed index without a Go checkout or toolchain.
