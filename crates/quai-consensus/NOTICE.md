# Consensus compatibility sources and attribution

This crate follows Dominant Strategies go-quai revision
`33db82f2891b267ba898ca38bd2dd8bf75cbb072`:
https://github.com/dominant-strategies/go-quai.
It is GPL-3.0-only; the GPL version 3 license is retained in LICENSE.

Native light-cache, dataset-item, ProgPoW and KAWPOW kernels are adapted from
consensus/progpow and consensus/kawpow. Those source files retain Copyright
2015, 2019 The go-ethereum Authors and GNU LGPL version 3 or (at your option)
any later version notices, without warranty. Preserve those attributions and
license terms when distributing derived source or binaries. The KAWPOW state
construction also follows the go-pow implementation identified by the pinned
source at https://github.com/Naikorasu/go-pow/commit/8f08901ee0527f11269f54a0ef8583f7a8270e78.

Rule projections follow common/big.go, core/poem.go,
core/headerchain_validation.go, core/headerchain.go, core/block_validator.go,
consensus/consensus.go, consensus/misc/statefee.go and params rules.
Exact BinaryLog normalization uses the native execution crate's existing
mathutil adaptation. Its BSD notice and the RustCrypto crypto-bigint MIT
notice are included with quai-execution and the standalone bundle. SHA3/
Keccak uses the pinned RustCrypto sha3 implementation.

Bounded auxiliary templates, donor Merkle commitments, quorum signatures,
zone header policy and conversion/base-fee arithmetic are implemented in the
declared profile. Complete economic state and cross-context header/block
policy, canonical history and fork choice remain unimplemented. Arithmetic oracle projections are separate from real
light-kernel proof vectors. No Go dependency or fake-proof mode is shipped.


RustCrypto sha2-0.10.9 MIT license option:

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


RustCrypto hmac-0.12.1 MIT license option:

Copyright (c) 2017 Artyom Pavlov

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


Signed auxiliary admission follows pinned core/types/auxpow.go,
auxpow_coinbase_utils.go, core/headerchain_validation.go and
crypto/musig2/musig2.go. It includes the actual 2-of-3 ordered aggregation,
template version mask, coinbase extraction and donor/Quai/Merkle bindings.
The MuSig2 verification uses RustCrypto k256 under the license option already
retained in the primitive notice. Lab keys in offline test fixtures are public
deterministic scalars 1, 2 and 3; the runtime defaults to the pinned protocol
public keys and never obtains trust keys from a proof. Full header/uncle/state
validity and canonical authority remain unimplemented.

Prime/region header components follow pinned core/headerchain_validation.go,
core/headerchain.go, core/poem.go, core/block_validator.go and params rules.
They preserve uint16 efficiency/threshold arithmetic, ETX eligibility bit order
and miner-difficulty EMA behavior. These checks use candidate history and
confer no economic validity or canonical chain authority.

Candidate append components follow pinned core/core.go, core/headerchain.go,
core/slice.go and core/types/block.go. They compute order/rank/routing,
parent manifest/interlink commitments and PCRC transitions over supplied
unverified history. Computed next manifests match Go rawdb persisted values;
the first-call Go helper return is recorded separately by the offline oracle.
No durable canonical chain publication or fork choice is implemented.

Genesis construction and initialization follow pinned core/genesis.go,
core/types/block.go, core/slice.go, common/types.go, common/address.go and
core/headerchain_validation.go. The native library constructs canonical
unverified payloads and candidate seeds/branches; it does not replay genesis
allocations or commit canonical economic state. Go disk bytes and canonical
address reconstruction are distinct, documented compatibility boundaries.

Candidate PoEM scores and coordinator projections follow pinned core/poem.go
and cmd/utils/hierarchical_coordinator.go. Native proof/header/body component
checks precede scoring. NodeSet and pending operations retain explicit source
location-name aliasing in the legacy compatibility view; neither view selects
or commits a canonical chain. The inherited copyright and license notices
above continue to apply.

Bounded global eligibility components follow pinned coordinator IsAncestor,
calculateFrontierPoints and PCRC gates. Imported multi-context views are bounded
and immutable per key. Missing ancestry is unavailable; the native library does
not reproduce the source's stale-nil-error success when a required prime header
is missing. These components confer no canonical history or full economic
validity. Runtime leader selection/generation integration and durable fork choice
remain unimplemented. Bounded recent admission, leader search/retries,
pending-frontier reapplication and virtual scheduling now follow pinned
cmd/utils/hierarchical_coordinator.go. Native node entropy binds proof and
body commitments; supplied history remains unverified.

Candidate selection retains equal-entropy ordering through a Rust adaptation
of Go 1.24.5 sort/zsortfunc.go and sort/sort.go.
Copyright 2022 The Go Authors. All rights reserved.

Copyright 2009 The Go Authors.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are
met:

   * Redistributions of source code must retain the above copyright
notice, this list of conditions and the following disclaimer.
   * Redistributions in binary form must reproduce the above
copyright notice, this list of conditions and the following disclaimer
in the documentation and/or other materials provided with the
distribution.
   * Neither the name of Google LLC nor the names of its
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

Pre-KAWPOW full-target uncle entropy follows core/poem.go: native full proofs,
integer entropy excess and ancestry-distance discounts. Weak pre-fork share
entropy requires separate floating-point compatibility qualification.

Weak-share discounting follows core/poem.go and common/math/big.go in the pinned
go-quai source, reproducing the cubic polynomial and binary nearest-even
rounding sequence with native bounded integers. Go math/big is used only by the
explicit offline oracle. The strong_uncles module remains a compatibility alias.
