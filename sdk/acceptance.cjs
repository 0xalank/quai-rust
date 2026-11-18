const fs = require('node:fs');
const path = require('node:path');
const [alias, url, profileFile, outputFile, subset = 'all'] = process.argv.slice(2);
const q = require(alias);
const profile = JSON.parse(fs.readFileSync(profileFile, 'utf8'));
const provider = new q.JsonRpcProvider(url, undefined, {
  usePathing: false, cacheTimeout: -1, batchMaxCount: 1,
});
const account = profile.account || '0x0010000000000000000000000000000000000001';
const contract = profile.contract || '0x0010000000000000000000000000000000000002';
const qi = profile.qi_address || '0x0080000000000000000000000000000000000001';
const zone = q.Zone.Cyprus1;
const shard = q.Shard.Cyprus1;
const normalize = value => JSON.parse(JSON.stringify(value, (_, v) => typeof v === 'bigint' ? v.toString() : v));
const results = [];
async function check(name, operation, requiredSuccess = true) {
  try {
    const value = await Promise.race([
      operation(), new Promise((_, reject) => { const timer = setTimeout(() => reject(new Error('acceptance deadline')), 8000); timer.unref(); }),
    ]);
    results.push({name, ok: true, value: normalize(value), requiredSuccess});
    if (process.env.SDK_DEBUG) console.error(name + ': pass');
  } catch (error) {
    if (process.env.SDK_DEBUG) console.error(name + ': ' + error.message);
    results.push({name, ok: false, requiredSuccess, error: {
      code: error.code || null, action: error.action || null,
      shortMessage: error.shortMessage || error.message,
      reason: error.reason || null, data: error.data || null,
    }});
  }
}
(async () => {
  await check('network', async () => ({chainId: (await provider.getNetwork()).chainId}));
  await check('height', () => provider.getBlockNumber(shard));
  if (subset === 'all') {
  await check('balance', () => provider.getBalance(account));
  await check('nonce', () => provider.getTransactionCount(account));
  await check('pending-nonce', () => provider.getTransactionCount(account, 'pending'));
  await check('code', () => provider.getCode(contract));
  await check('storage', () => provider.getStorage(contract, '0x0'));
  await check('call', () => provider.call({from: account, to: contract}), !!profile.funded);
  await check('access-list', () => provider.createAccessList({from: account, to: contract}), !!profile.funded);
  await check('estimate', () => provider.estimateGas({from: account, to: contract}), !!profile.funded);
  await check('qi-outpoints', () => provider.getOutpointsByAddress(qi));
  if (profile.funded) {
    const qiRequest = {from: qi, txType: 2,
      txIn: [{previousOutPoint: {txHash: profile.utxo_hash, index: '0x0'}, pubKey: '0x02' + '11'.repeat(32)}],
      txOut: [{denomination: '0x0', address: qi, lock: '0x0'}]};
    await check('qi-fee-valid', () => provider.estimateFeeForQi(qiRequest));
  }

  }
  await check('block', async () => {
    const block = await provider.getBlock(shard, 'latest', true);
    return block && block.toJSON();
  });
  await check('block-by-hash', async () => { const block = await provider.getBlock(shard, profile.head_hash || profile.genesis_hash, true); return block && block.toJSON(); });
  await check('missing-receipt', () => provider.getTransactionReceipt('0x' + '00'.repeat(32)));
  if (profile.receipt_hashes) {
    for (const hash of profile.receipt_hashes) {
      await check('receipt-' + hash, async () => {
        const value = await provider.getTransactionReceipt(hash);
        return value && value.toJSON();
      });
    }
  }
  if (subset === 'all') {
  await check('invalid-block-tag', () => provider.send('quai_getBalance', [account, 'finalized'], shard), false);
  await check('qi-fee-invalid', () => provider.send('quai_estimateFeeForQi', [{}], shard), false);
  }
  let directory = path.dirname(require.resolve(alias));
  while (!fs.existsSync(path.join(directory, 'package.json')) || JSON.parse(fs.readFileSync(path.join(directory, 'package.json'))).name !== 'quais') directory = path.dirname(directory);
  const manifest = JSON.parse(fs.readFileSync(path.join(directory, 'package.json')));
  const report = {package: alias, packageVersion: manifest.version, runtimeVersion: q.version,
    subset, options: {usePathing: false, cacheTimeout: -1, batchMaxCount: 1}, results};
  fs.writeFileSync(outputFile, JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify({package: alias, cases: results.length, requiredFailures: results.filter(v => v.requiredSuccess && !v.ok).map(v => v.name)}));
})().catch(error => { console.error(error); process.exitCode = 1; }).finally(() => provider.destroy());
