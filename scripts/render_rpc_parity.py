#!/usr/bin/env python3
"""Render the RPC parity table (docs/rpc-parity.md) and its machine-readable
form (compatibility/rpc-parity.json) from the go-quai v0.57.0 inventory
(fixtures/protocol/rpc-inventory.json, scripts/gen_rpc_inventory.py), the
class of each method (below) and what the Rust node serves
(crates/quai-rpc/src/native/mod.rs METHODS, native/filters.rs SUBSCRIPTIONS).

    scripts/render_rpc_parity.py --write
    scripts/render_rpc_parity.py --check

The Rust test `native::parity_table_tests` checks the "rust" field of every
row against METHODS and SUBSCRIPTIONS.
"""
import argparse, json, re
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
m = p.add_mutually_exclusive_group(required=True)
m.add_argument('--write', action='store_true')
m.add_argument('--check', action='store_true')
a = p.parse_args()

inv = json.loads((ROOT / 'fixtures/protocol/rpc-inventory.json').read_text())


def rust_list(path, const):
    src = (ROOT / path).read_text()
    body = re.search(r'pub const ' + const + r': &\[&str\] = &\[(.*?)\];', src, re.S).group(1)
    return re.findall(r'"([^"]+)"', body)


METHODS = set(rust_list('crates/quai-rpc/src/native/mod.rs', 'METHODS'))
SUBSCRIPTIONS = set(rust_list('crates/quai-rpc/src/native/filters.rs', 'SUBSCRIPTIONS'))

# --- classes -----------------------------------------------------------------
# (a) read-only chain or state; (b) mining, block templates or Stratum;
# (c) admin or debug; (d) personal or signer.
MINING_QUAI = {
    'getBlockTemplate', 'getPendingHeader', 'getPendingWorkShares', 'getMiningInfo',
    'calcOrder', 'getWorkShareP2PThreshold', 'receiveMinedHeader', 'receiveRawWorkShare',
    'receiveWorkShare', 'submitScryptBlock', 'submitKawpowBlock', 'submitShaBlock',
    'signAuxTemplate', 'submitAuxTemplate', 'getSubsidyChainHeight',
    'getDonorChainInfoForWorkshare', 'getQuaiHeaderForDonorHash',
    'getCoinbaseTxForWorkShareHash',
}
MINING_SUBSCRIPTIONS = {'pendingHeader', 'blockTemplateUpdates', 'newWorkshares', 'newWorksharesV2'}
SIGNER_NAMES = {
    'eth_sendTransaction', 'eth_accounts', 'eth_requestAccounts', 'eth_sign',
    'eth_signTransaction', 'eth_signTypedData_v4', 'personal_sign', 'personal_listAccounts',
    'personal_unlockAccount', 'personal_sendTransaction', 'quai_accounts',
    'quai_requestAccounts', 'quai_sendTransaction', 'quai_sign', 'quai_signTransaction',
}


def klass(x):
    ns, name = x['namespace'], x['name']
    if x['kind'] == 'subscription':
        return 'b' if name in MINING_SUBSCRIPTIONS else 'a'
    if ns in ('admin', 'debug'):
        return 'c'
    if ns in ('miner', 'workshare'):
        return 'b'
    if ns == 'quai' and name == 'setWorkShareP2PThreshold':
        return 'c'
    if ns == 'quai' and name in MINING_QUAI:
        return 'b'
    if ns == 'eth' and name in ('mining', 'primaryCoinbase'):
        return 'b'
    return 'a'


def outside_class(name):
    if name in SIGNER_NAMES:
        return 'd'
    ns = name.split('_')[0]
    if ns in ('admin',):
        return 'c'
    if name in ('miner_start', 'quai_mining', 'quai_hashrate', 'eth_hashrate', 'eth_coinbase'):
        return 'b'
    return 'a'


# --- notes -------------------------------------------------------------------
INDEX = '`--node.index-address-utxos` (default off)'
NOTES = {
    'quai_getLockedBalance': f'Quai address: `rawdb.ReadLockedBalance`, written only with {INDEX}; '
                             'a default node answers 0x0 and so does Rust',
    'quai_getOutpointsByAddress': f'`rawdb.ReadAddressUTXOs`, written only with {INDEX}: a default '
                                  'node answers []; Rust answers from its UTXO set, as an indexing node',
    'quai_getBalance': f'Qi address: the address UTXO index ({INDEX}); a default node answers 0x0, '
                       'Rust answers from its UTXO set, as an indexing node',
    'quai_getSupplyAnalyticsForBlock': f'`rawdb.ReadSupplyAnalyticsForBlock` is written only with {INDEX}; '
                                       'a default node reads zeros and subtracts the burn address balance',
    'quai_rewardAnalytics': f'total supply from the supply index ({INDEX}); a default node answers '
                            '"0" after its walk of the genesis unlocks',
    'quai_getKQuaiAndUpdateBit': 'zone [0 0] only; v0.57.0 answers params.ExchangeRate and 1 for any stored block',
    'quai_clientVersion': 'node identity: Go answers `go-quai/<VERSION>-<commit>`',
    'eth_clientVersion': 'node identity: Go answers `go-quai/<VERSION>-<commit>`',
    'rpc_modules': 'the namespaces of the endpoint and "1.0"',
    'net_peerCount': 'libp2p peers; 0 without a p2p backend',
    'net_peerCountByDirection': 'libp2p peers by direction; zeros without a p2p backend',
    'txpool_getRollingFeeInfo': 'in-memory min, max and average fee of the blocks this process executed; '
                                'an error until it executes a block with transactions',
    'quai_getTransactionByBlockHashAndIndex': 'its third parameter, a `common.Location`, is required',
    'eth_getTransactionByBlockHashAndIndex': 'its third parameter, a `common.Location`, is required',
    'quai_sendRawTransaction': 'pool submission (not read-only)',
    'eth_sendRawTransaction': 'pool submission (not read-only)',
    'quai_receiveTxFromPoolSharingClient': 'pool submission (not read-only)',
    'eth_receiveTxFromPoolSharingClient': 'pool submission (not read-only)',
    'quai_setWorkShareP2PThreshold': 'a setter on the public API',
}
# Class (b): what a default go-quai zone node answers with no miner attached.
# Every zone node builds a pending header on each new head and every second
# (cmd/utils/hierarchical_coordinator.go ComputePendingHeaders, core/worker.go
# asyncStateLoop, core/slice.go SetBestPh); no flag gates it.
NOTES_B = {
    'quai_getPendingHeader': 'no miner needed: "0x" protobuf of the best pending header (header only, '
                             'AuxPow nil), rebuilt on each head and each second; "method handler crashed" '
                             'before the first (CopyWorkObject(nil)); a Progpow job, invalid after the '
                             'KawPow transition. Quais.js exposes `getPendingHeader()` and calls it nowhere; '
                             'Pelagus does not use it',
    'quai_getBlockTemplate': 'no miner needed: a getblocktemplate map from the pending header and the newest '
                             'aux template (hardcoded defaults before any arrive); each call adds entries '
                             'to two 1,000-entry caches; no wallet use',
    'quai_getPendingWorkShares': 'gossip workshares by PoW name ({} without any); no wallet use',
    'quai_getMiningInfo': 'head and last-15-minute figures; its quaiSupplyTotal is the negated burn balance '
                          'without the supply index; no wallet use',
    'quai_calcOrder': 'the order (0x0, 0x1, 0x2) of a submitted work object after its seal check; no wallet use',
    'quai_getWorkShareP2PThreshold': '"0x7"; no wallet use',
    'quai_receiveMinedHeader': 'miner submission; no wallet use',
    'quai_receiveRawWorkShare': 'miner submission; no wallet use',
    'quai_receiveWorkShare': 'miner submission; no wallet use',
    'quai_submitScryptBlock': '"could not get the pending block body for cache key 0x..." unless this node '
                              'issued the template; no wallet use',
    'quai_submitKawpowBlock': 'as quai_submitScryptBlock',
    'quai_submitShaBlock': 'as quai_submitScryptBlock',
    'quai_signAuxTemplate': '"MuSig2 keys not configured - set MUSIG2_PRIVKEY environment variable"; '
                            'no wallet use',
    'quai_submitAuxTemplate': 'null for a template with a valid 2-of-3 MuSig2 signature, then gossip; '
                              'no wallet use',
    'quai_getSubsidyChainHeight': 'heights of the newest aux templates (defaults before any arrive); '
                                  'no wallet use',
    'quai_getDonorChainInfoForWorkshare': f'needs {INDEX}: a default node answers "block not found for '
                                          'work share hash"',
    'quai_getQuaiHeaderForDonorHash': f'needs {INDEX}: a default node answers "no quai header found for '
                                      'donor hash"',
    'quai_getCoinbaseTxForWorkShareHash': f'needs {INDEX}: a default node answers "no block found for '
                                          'workshare hash"',
    'eth_mining': 'true: the worker always runs',
    'eth_primaryCoinbase': 'the coinbase of the last pending header (one of the two default coinbases)',
    'workshare_getWorkShareThreshold': '13, a JSON number',
    'workshare_receiveSubWorkshare': 'sub-workshare intake',
    'quai_subscribe:pendingHeader': 'fires on every new pending header (each head and each second); '
                                    'a base64 protobuf payload',
    'eth_subscribe:pendingHeader': 'as quai_subscribe "pendingHeader"',
    'quai_subscribe:blockTemplateUpdates': 'the template, then on changes and every 5 s',
    'eth_subscribe:blockTemplateUpdates': 'as quai_subscribe "blockTemplateUpdates"',
    'quai_subscribe:newWorkshares': 'gossip workshares (fires with no local miner)',
    'eth_subscribe:newWorkshares': 'as quai_subscribe "newWorkshares"',
    'quai_subscribe:newWorksharesV2': 'gossip workshares, RPC version v2',
    'eth_subscribe:newWorksharesV2': 'as quai_subscribe "newWorksharesV2"',
}
NOTES.update(NOTES_B)

# --- rows --------------------------------------------------------------------


def key(x):
    return x['method'] + (':' + x['name'] if x['kind'] == 'subscription' else '')


def rust_serves(x):
    if x['kind'] == 'subscription':
        return x['method'] in METHODS and x['name'] in SUBSCRIPTIONS
    return x['method'] in METHODS


rows = []
for x in inv['methods']:
    rows.append({
        'key': key(x), 'method': x['method'], 'namespace': x['namespace'], 'name': x['name'],
        'kind': x['kind'], 'source': x['source'], 'public': x['public'],
        'http': x['http']['served'], 'ws': x['ws']['served'], 'class': klass(x),
        'rust': rust_serves(x), 'note': NOTES.get(key(x), ''),
    })
outside = []
for x in inv['outside']:
    assert not x['http']['served'] and not x['ws']['served'], x
    outside.append({'method': x['method'], 'class': outside_class(x['method']),
                    'rust': x['method'] in METHODS})
keys = {r['key'] for r in rows}
methods = {r['method'] for r in rows}
unknown = sorted(n for n in METHODS if n not in methods and not n.endswith('_unsubscribe'))
assert not unknown, f'Rust serves methods that go-quai does not register: {unknown}'


def flag(r, transport):
    if r[transport]:
        return 'yes'
    if r['kind'] == 'subscription' and transport == 'http':
        return 'no (WebSocket only)'
    return f"no (`--rpc.{transport}-api {r['namespace']}`)"


default = lambda r: r['http'] or r['ws']
counts = Counter()
for r in rows:
    where = 'http' if r['http'] else ('ws' if r['ws'] else 'off')
    counts[(r['class'], where, r['rust'])] += 1
summary = {
    'methods': len(rows),
    'by_class': {c: sum(1 for r in rows if r['class'] == c) for c in 'abcd'},
    'default_http': sum(r['http'] for r in rows),
    'default_ws': sum(r['ws'] for r in rows),
    'default_any': sum(default(r) for r in rows),
    'rust_served': sum(r['rust'] for r in rows),
    'class_a_default_missing': sorted(r['key'] for r in rows
                                      if r['class'] == 'a' and default(r) and not r['rust']),
}
machine = {'reference_revision': inv['reference_revision'],
           'inventory': 'fixtures/protocol/rpc-inventory.json',
           'summary': summary, 'rows': rows, 'outside': outside}
encoded_json = json.dumps(machine, indent=1, sort_keys=True) + '\n'

# --- markdown ----------------------------------------------------------------
cfg = inv['config']
mods = inv['rpc_modules']
L = []
w = L.append
w('# JSON-RPC parity with go-quai v0.57.0')
w('')
w('Generated by `scripts/render_rpc_parity.py` from the Go oracle inventory')
w('`fixtures/protocol/rpc-inventory.json` (`scripts/gen_rpc_inventory.py`,')
w('`reference/rpc-inventory`). Do not edit by hand.')
w('')
w('## How the inventory is made')
w('')
w('- The API list is the `rpc.API` literals of `quaiapi.GetAPIs`')
w('  (internal/quaiapi/backend.go:197) and `Quai.APIs` (quai/backend.go:294),')
w('  parsed from the source in registration order, with the zone-only block.')
w('  The registry of go-quai\'s `rpc.Server` (rpc/service.go) lists every method')
w('  and subscription; each row names the Go function that serves it.')
w('- "HTTP" and "WS" say whether a zone node started with no flags serves the')
w('  name. The oracle builds `makeConfigNode`\'s configuration (cmd/utils/cmd.go')
w('  `defaultNodeConfig`, then `SetNodeConfig` over the start command\'s flags),')
w('  starts the production `node.Node` with it on loopback, and probes every')
w('  name: `rpc.Server` answers -32602 for a registered name before it runs the')
w('  handler and -32601 for an unknown one. No handler runs.')
w('- Classes: (a) read-only chain or state, with the pool submissions;')
w('  (b) mining, block templates or Stratum; (c) admin or debug; (d) personal')
w('  or signer.')
w('')
w('## Default endpoints of a go-quai zone node')
w('')
w(f"- HTTP on {cfg['http']['host']}:{cfg['http']['port']} (zone [0 0]): modules "
  f"`{', '.join(cfg['http']['modules'])}` (node/defaults.go plus `defaultNodeConfig`).")
w('  No API has the namespace "web3", so the endpoint serves')
w(f"  {', '.join('`' + k + '`' for k in sorted(mods['http']))}.")
w('  `--rpc.http-api` replaces the list.')
w(f"- WebSocket on {cfg['ws']['host']}:{cfg['ws']['port']}: `setWS` always sets the")
w('  modules from `--rpc.ws-api`, whose default is empty, and `RegisterApis`')
w('  (node/rpcstack.go:490) then registers every public API:')
w(f"  {', '.join('`' + k + '`' for k in sorted(mods['ws']))}.")
w('  So `eth_*`, `txpool_*`, `workshare_*` and the public `debug_*` methods are')
w('  served by default, but only over WebSocket.')
w('- Subscriptions (`*_subscribe`) work over WebSocket only; over HTTP')
w('  `rpc.Server` answers "notifications not supported".')
w('- A `*_unsubscribe` call reaches one handler for every namespace, even an')
w('  unknown one (rpc/handler.go `handleCall`).')
w('- The Rust node serves all its methods over HTTP and over WebSocket on one')
w('  listen address. It therefore also serves `txpool_*` and `eth_*` over')
w('  HTTP, as go-quai does with `--rpc.http-api` naming them.')
w('')
w('## Counts')
w('')
w(f"go-quai v0.57.0 registers {len(rows)} names for a zone node "
  f"({sum(r['kind'] == 'method' for r in rows)} methods and "
  f"{sum(r['kind'] == 'subscription' for r in rows)} subscriptions over the `quai` and `eth` "
  f"namespaces); {summary['default_http']} are served over HTTP by default and "
  f"{summary['default_ws']} over WebSocket.")
w('')
w('| Class | Registered | Default HTTP and WS: Rust serves | missing | Default WS only: Rust serves | missing | Not default: Rust serves | missing |')
w('|---|---|---|---|---|---|---|---|')
for c in 'abcd':
    reg = sum(1 for r in rows if r['class'] == c)
    cell = lambda where, served: counts[(c, where, served)]
    w(f"| ({c}) | {reg} | {cell('http', True)} | {cell('http', False)} | {cell('ws', True)} | "
      f"{cell('ws', False)} | {cell('off', True)} | {cell('off', False)} |")
tot = lambda where, served: sum(counts[(c, where, served)] for c in 'abcd')
w(f"| all | {len(rows)} | {tot('http', True)} | {tot('http', False)} | {tot('ws', True)} | "
  f"{tot('ws', False)} | {tot('off', True)} | {tot('off', False)} |")
w('')
miss = summary['class_a_default_missing']
w(f"Class (a) names that a default node serves and Rust does not: {len(miss)}"
  + (': ' + ', '.join('`' + k + '`' for k in miss) + '.' if miss else '.'))
w('')
w('## Methods and subscriptions')
w('')
w('Subscriptions are listed as `<namespace>_subscribe` with their name.')
w('')
for ns in sorted({r['namespace'] for r in rows}):
    w(f'### {ns}')
    w('')
    w('| Name | Go source | HTTP | WS | Rust | Class | Note |')
    w('|---|---|---|---|---|---|---|')
    for r in [r for r in rows if r['namespace'] == ns]:
        name = f"`{r['method']}` \"{r['name']}\"" if r['kind'] == 'subscription' else f"`{r['method']}`"
        w(f"| {name} | {r['source']} | {flag(r, 'http')} | {flag(r, 'ws')} | "
          f"{'served' if r['rust'] else 'missing'} | ({r['class']}) | {r['note']} |")
    w('')
w('## Where the Rust node answers differently')
w('')
w('Each served name answers with go-quai\'s JSON and error texts over the Go')
w('oracles (`fixtures/protocol/rpc-parity.json`, `rpc-calls.json`,')
w('`filters.json`, `qi-rpc.json`, `txpool.json`, `fixtures/native-rpc`).')
w('What still differs:')
w('')
w('- Transports: the Rust node serves every name above over HTTP and')
w('  WebSocket on one address; a default go-quai node serves `eth_*` and')
w('  `txpool_*` over WebSocket only.')
w('- `quai_clientVersion` and `eth_clientVersion` name the Rust node')
w('  (`quai-rust/v<version>`); `rpc_modules` lists the namespaces the Rust')
w('  node serves (the same on both transports).')
w('- The address UTXO index (`--node.index-address-utxos`, off by default):')
w('  `quai_getOutpointsByAddress` and the')
w('  Qi balances answer from the UTXO set, as an indexing go-quai node does;')
w('  `quai_getLockedBalance` of a Quai address, `quai_getSupplyAnalyticsForBlock`')
w('  and `quai_rewardAnalytics` answer as a default node does.')
w('- Live events: the `quai-node rpc` view is frozen and sends no event, as a')
w('  go-quai node that is not syncing. `quai-node run` publishes its heads')
w('  ("newHeads", "newHeadsV2", "logs", block filters and "accesses" by')
w('  transaction) but not yet its appended blocks ("newChainBlocks",')
w('  "newChainBlocksV2"), its unlocks ("accesses" by unlock), its rolling fee')
w('  window (`txpool_getRollingFeeInfo` answers go-quai\'s "no transactions')
w('  processed" error) or its peer counts (`net_peerCount` answers 0x0, as')
w('  go-quai without a p2p backend).')
w('- "accesses" matches the sender of a Quai transaction; go-quai matches it')
w('  only when the transaction\'s sender is cached (`Transaction.From`), as it')
w('  is in a block the node processed.')
w('- go-quai drops events that a busy subscriber cannot take (non-blocking')
w('  sends into channels of 0 or 10 entries); the Rust node queues up to')
w('  4,096 messages per connection.')
w('')
w('## Names that go-quai v0.57.0 does not register')
w('')
w('No flag serves these on a go-quai v0.57.0 node: each answers -32601 over')
w('HTTP and WebSocket. Signer methods (d) belong to the wallet (Pelagus')
w('answers them itself); the node never holds keys.')
w('')
w('| Name | Class | Rust |')
w('|---|---|---|')
for o in outside:
    w(f"| `{o['method']}` | ({o['class']}) | {'served' if o['rust'] else 'not served'} |")
w('')
w('## Mining phase (class b)')
w('')
w('Not ported here: the mining phase comes last. A default go-quai zone node')
w('builds a pending header on every new head and every second whether or not')
w('a miner is attached, so most class (b) methods answer on a non-mining node')
w('(the notes above say what). No wallet needs them: Quais.js and Pelagus')
w('use none (Quais.js exposes `getPendingHeader()` without calling it).')
w('')
w('What the Rust node needs to serve them:')
w('')
w('1. A pending-header builder (core/worker.go, core/slice.go): the prime,')
w('   region and zone header parts and their combination; transaction')
w('   selection from the Quai and Qi pools with the Qi fee conversion, inbound')
w('   ETXs, the ETX set and the gas and state budgets; uncle and workshare')
w('   selection from three 1,000-entry caches; coinbase ETXs for shares at')
w('   depth 3 to 4; `FinalizeAssemble` (EVM, UTXO and ETX-set roots, receipts,')
w('   manifest); the coinbase choice and lock byte; the KawPow AuxPow')
w('   placeholder.')
w('2. The pending-header cache and its triggers: the best pending header in')
w('   memory and on disk, rebuilt on each head, every second and when local')
w('   transactions wait, with the "pendingHeader" feed.')
w('3. The body and AuxPow caches keyed by seal hash (and seal hash with the')
w('   signature time), kept across restarts.')
w('4. Aux templates: gossip intake with the 2-of-3 MuSig2 check, the newest')
w('   template per algorithm with the hardcoded defaults, the donor coinbase')
w('   and header builders (Ravencoin 120 bytes, BCH and Litecoin 80 bytes),')
w('   the coinb1/coinb2 split, merkle roots and the getblocktemplate JSON.')
w('5. Workshare intake: gossip validation with the runtime P2P threshold,')
w('   `AddWorkShare` and its feed ("newWorkshares").')
w('6. Submissions: the protobuf work-object codecs, workshare classification,')
w('   donor block parsing (`SubmitBlock`), `CalcOrder`, local block')
w('   construction and gossip publication.')
w('7. Proof of work: ProgPoW and KAWPOW with the DAG, SHA256d and scrypt donor')
w('   hashes, and the entropy thresholds.')
w('8. MuSig2 verification (signing only on a signer node), and the optional')
w('   `--node.index-address-utxos` indexes (donor hash, workshare hash).')
w('')
w('A default go-quai node without a miner also does not relay wallet')
w('transactions: gossip has no transaction topic, so a local transaction')
w('leaves the node only inside a share mined on its template, or through')
w('`--txpool.sharing-clients` (core/tx_pool.go, core/slice.go).')
w('')
encoded_md = '\n'.join(L) + '\n'

targets = {ROOT / 'compatibility/rpc-parity.json': encoded_json, ROOT / 'docs/rpc-parity.md': encoded_md}
for path, text in targets.items():
    if a.write:
        path.write_text(text)
    elif path.read_text() != text:
        raise SystemExit(f'{path.relative_to(ROOT)} is out of date')
print(json.dumps({'passed': True, 'mode': 'write' if a.write else 'check', **{
    k: v for k, v in summary.items() if k != 'class_a_default_missing'},
    'class_a_default_missing': len(miss)}))
