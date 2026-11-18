const fs = require('node:fs');
const [alias, url, profileFile, outputFile] = process.argv.slice(2);
const q = require(alias);
const profile = JSON.parse(fs.readFileSync(profileFile, 'utf8'));
const provider = new q.JsonRpcProvider(url, undefined, {usePathing: false, cacheTimeout: -1, batchMaxCount: 1});
const results = [];
async function check(name, action) {
  const value = await Promise.race([action(), new Promise((_, reject) => {const timer=setTimeout(()=>reject(new Error('SDK deadline')),8000);timer.unref();})]);
  results.push({name,value:JSON.parse(JSON.stringify(value,(_,v)=>typeof v==='bigint'?v.toString():v))});
}
(async () => {
  await check('network',async()=>({chainId:(await provider.getNetwork()).chainId}));
  for (const hash of profile.receipt_hashes) await check('receipt-'+hash,async()=>{const value=await provider.getTransactionReceipt(hash);return value&&value.toJSON();});
  for (const [name,filter] of [
    ['logs-block',{blockHash:profile.head_hash}],
    ['logs-address',{blockHash:profile.head_hash,address:profile.contract}],
    ['logs-topic',{blockHash:profile.head_hash,topics:[[profile.topic]]}],
    ['logs-empty',{blockHash:profile.head_hash,topics:['0x'+'ee'.repeat(32)]}],
  ]) await check(name,async()=>(await provider.getLogs({...filter,nodeLocation:[0,0]})).map(log=>log.toJSON()));
  fs.writeFileSync(outputFile,JSON.stringify({alias,runtimeVersion:q.version,results},null,2)+'\n');
})().catch(error=>{console.error(error);process.exitCode=1;}).finally(()=>provider.destroy());
