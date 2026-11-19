// Quais.js against the native RPC's filters and WebSocket subscriptions,
// on loopback only: crates/quai-node/examples/fixture-rpc.rs serves the test
// chain of fixtures/protocol/filters.json and moves its head (b3, b4, b7, a
// reorg to c6, c7, c8, back to b8). This script listens with a
// WebSocketProvider ("block" through "newHeads", a log filter through "logs")
// and a JsonRpcProvider (a log filter through quai_newFilter and
// quai_getFilterChanges, "block" by polling), then reads the logs and the head
// block, and writes what each listener received.
//
//   node sdk/fixture-subscriptions.cjs QUAIS_LIB HTTP_URL WS_URL SECONDS OUT.json
const fs = require('node:fs');
const [lib, httpUrl, wsUrl, seconds, out] = process.argv.slice(2);
const q = require(lib);
const zone = q.Zone.Cyprus1;
const shard = q.Shard.Cyprus1;
const address = '0x00110000000000000000000000000000000000a1';
const normalize = (v) => JSON.parse(JSON.stringify(v, (_, x) => (typeof x === 'bigint' ? x.toString() : x)));
const got = { version: q.version, wsBlocks: [], wsLogs: [], httpBlocks: [], httpLogs: [], errors: [] };
const log = (l) => ({ block: l.blockNumber, index: l.index, tx: l.transactionHash, topics: l.topics, removed: l.removed });
async function step(name, f) {
  try {
    await f();
  } catch (e) {
    got.errors.push({ step: name, message: e.shortMessage || e.message });
  }
}
(async () => {
  const http = new q.JsonRpcProvider(httpUrl, undefined, { usePathing: false });
  const ws = new q.WebSocketProvider(wsUrl, undefined, { usePathing: false });
  await step('network', async () => {
    got.chainId = (await http.getNetwork()).chainId;
  });
  await step('ws-block', () => ws.on('block', (n) => got.wsBlocks.push(n), zone));
  await step('ws-logs', () => ws.on({ address }, (l) => got.wsLogs.push(log(l)), zone));
  await step('http-logs', () => http.on({ address }, (l) => got.httpLogs.push(log(l)), zone));
  await step('http-block', () => http.on('block', (n) => got.httpBlocks.push(n), zone));
  await new Promise((r) => setTimeout(r, Number(seconds) * 1000));
  await step('get-logs', async () => {
    got.logs = (await http.getLogs({ address, fromBlock: 0, toBlock: 'latest', nodeLocation: [0, 0] })).map(log);
  });
  await step('head-block', async () => {
    const b = await http.getBlock(shard, 'latest', true);
    got.head = { number: b.woHeader.number, hash: b.hash, transactions: b.transactions.length };
  });
  await step('teardown', async () => {
    await ws.removeAllListeners();
    await http.removeAllListeners();
  });
  fs.writeFileSync(out, JSON.stringify(normalize(got), null, 1));
  console.log(JSON.stringify(normalize({
    chainId: got.chainId, wsBlocks: got.wsBlocks, httpBlocks: got.httpBlocks,
    wsLogs: got.wsLogs.length, httpLogs: got.httpLogs.length, logs: (got.logs || []).length,
    head: got.head, errors: got.errors,
  })));
  process.exit(0);
})();
