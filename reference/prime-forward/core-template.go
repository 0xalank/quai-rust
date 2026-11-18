// Controlled history around the unmodified official parent-economics verifier.
package core
import (
 "encoding/hex"
 "encoding/binary"
 "crypto/sha256"
 "errors"
 "sort"
 "github.com/dominant-strategies/go-quai/ethdb"
 "fmt"
 "math/big"
 "regexp"
 "strings"
 "github.com/dominant-strategies/go-quai/common"
 "github.com/dominant-strategies/go-quai/consensus/misc"
 "github.com/dominant-strategies/go-quai/core/rawdb"
 "github.com/dominant-strategies/go-quai/core/types"
 "github.com/dominant-strategies/go-quai/log"
 "github.com/dominant-strategies/go-quai/params"
 "github.com/dominant-strategies/go-quai/trie"
 lru "github.com/hashicorp/golang-lru/v2"
 "google.golang.org/protobuf/proto"
)
func peWire(w *types.WorkObject) string {
 p,e:=w.ProtoEncode(types.BlockObject);if e!=nil {panic(e)};b,e:=proto.Marshal(p);if e!=nil {panic(e)};return "0x"+hex.EncodeToString(b)
}
func peTx(t *types.Transaction) string {
 p,e:=t.ProtoEncode();if e!=nil {panic(e)};b,e:=proto.Marshal(p);if e!=nil {panic(e)};return "0x"+hex.EncodeToString(b)
}
// Bind each supplied post-fork object to its own donor coinbase before hashing.
// This constructs a component input; proof work and signatures are not asserted.
func peBind(w *types.WorkObject) {
 h:=w.WorkObjectHeader();h.SetHeaderHash(w.Header().Hash())
 if w.PrimeTerminusNumber().Uint64()<params.KawPowForkBlock {return}
 if h.KawpowDifficulty()==nil {
  z:=func()*big.Int{return new(big.Int)}
  h.SetKawpowDifficulty(z());h.SetShaShareTarget(z());h.SetScryptShareTarget(z())
  h.SetShaDiffAndCount(types.NewPowShareDiffAndCount(z(),z(),z()));h.SetScryptDiffAndCount(types.NewPowShareDiffAndCount(z(),z(),z()))
 }
 t:=types.DefaultKawpowAuxTemplate()
 tx:=types.NewAuxPowCoinbaseTx(types.Kawpow,t.Height(),t.CoinbaseOut(),w.SealHash(),t.SignatureTime())
 mr:=types.CalculateMerkleRoot(types.Kawpow,tx,t.MerkleBranch())
 donor:=types.NewBlockHeader(types.Kawpow,int32(t.Version()),t.PrevHash(),mr,t.SignatureTime()+1,t.Bits(),0,t.Height())
 h.SetAuxPow(types.NewAuxPow(types.Kawpow,donor,t.AuxPow2(),t.Sigs(),t.MerkleBranch(),tx))
}
// Official economic branch and parent calculation, linked only in the explicit
// offline reference profile. go-quai is GPL-3.0; see this folder's LICENSE.
// Exact extraction hashes and the probe-only changes are recorded by the driver.
@@PARENT_PROBE@@
func NativePrimeForwardEconomicBranch(sl *Slice, header, block *types.WorkObject, newInboundEtxs types.Transactions, pending *ethdb.Batch) (types.Transactions,error) {
 nodeCtx:=sl.NodeCtx();batch:=sl.sliceDb.NewBatch()
@@APPEND_BRANCH@@
 *pending=batch
 return newInboundEtxs,nil
}
// This observer catches the reference branch's nil-refund panic without
// changing that branch or publishing its abandoned batch.
func pfObserveZeroPanic(sl *Slice,current *types.WorkObject,input types.Transactions)(observed bool,message string) {
 defer func(){if value:=recover();value!=nil {observed=true;message=fmt.Sprint(value)}}()
 var discarded ethdb.Batch
 _,err:=NativePrimeForwardEconomicBranch(sl,current,current,input,&discarded)
 if err!=nil {return false,err.Error()}
 return false,"no panic"
}
func pfDigest(set types.TokenChoiceSet) string {
 h:=sha256.New();var row [48]byte
 for _,c:=range set {binary.BigEndian.PutUint64(row[:8],c.Quai);binary.BigEndian.PutUint64(row[8:16],c.Qi);c.Diff.FillBytes(row[16:]);h.Write(row[:])}
 return "0x"+hex.EncodeToString(h.Sum(nil))
}
func pfSum(set types.TokenChoiceSet) string {n:=new(big.Int);for _,c:=range set {n.Add(n,c.Diff)};return n.String()}
func pfClone(txs types.Transactions) types.Transactions {out:=types.Transactions{};for _,tx:=range txs {out=append(out,types.NewTx(tx.Inner()))};return out}
func pfEncode(txs types.Transactions) []string {out:=[]string{};for _,tx:=range txs {out=append(out,peTx(tx))};return out}
func NativePrimeForwardVectors() map[string]interface{} {
 vectors:=[]interface{}{};mutations:=0;missing:=0;nilProbes:=0;totalSteps:=0
 heights:=[]uint64{262000,262001,262002,265999,266000,266001,284999,285000,285001,751999,752000,752001,771999,772000,1011199,1011200,1011201,1031199,1031200,1171499,1171500,1171501,1754999,1755000,1755001,2236999,2237000,2237001}
 local:=regexp.MustCompile(`local: ([0-9]+)\)`)
 for index:=0;index<256;index++ {
  db:=rawdb.NewMemoryDatabase(log.Global)
  cfg:=*params.TestChainConfig;cfg.Location=common.Location{}
  numbers,_:=lru.New[common.Hash,uint64](128)
  hc:=&HeaderChain{config:&cfg,headerDb:db,bc:NewTestBodyDb(db),numberCache:numbers,logger:log.Global}
  hc.bc.chainConfig=&cfg
  sl:=&Slice{hc:hc,sliceDb:db,config:&cfg,logger:log.Global}
  number:=heights[index%len(heights)]
  makeBlock:=func(n uint64,miner *big.Int) *types.WorkObject {
   w:=types.EmptyWorkObject(common.ZONE_CTX)
   w.SetNumber(new(big.Int).SetUint64(n),common.PRIME_CTX)
   w.SetNumber(new(big.Int).SetUint64(300000+uint64(index)),common.ZONE_CTX)
   w.WorkObjectHeader().SetNumber(new(big.Int).SetUint64(300000+uint64(index)))
   w.WorkObjectHeader().SetLocation(common.Location{0,0})
   w.WorkObjectHeader().SetPrimaryCoinbase(common.ZeroAddress(common.Location{0,0}))
   w.WorkObjectHeader().SetPrimeTerminusNumber(new(big.Int).SetUint64(n))
   w.WorkObjectHeader().SetDifficulty(big.NewInt(100000000003))
   w.Header().SetMinerDifficulty(new(big.Int).Set(miner))
   w.Header().SetExchangeRate(new(big.Int).Set(params.ExchangeRate))
   w.Header().SetConversionFlowAmount(new(big.Int).Set(params.StartingConversionFlowAmount))
   w.Header().SetKQuaiDiscount(big.NewInt(int64((index*7919)%100001)))
   w.Header().SetAvgTxFees(big.NewInt(int64(index)*997))
   peBind(w);return w
  }
  grand:=makeBlock(number-1,big.NewInt(900000000007+int64(index)*991))
  parent:=makeBlock(number,big.NewInt(1000000000039+int64(index)*997))
  if index%7==0 {parent.SetNumber(new(big.Int).SetUint64(params.BlocksPerYear+1),common.ZONE_CTX)}
  rate:=new(big.Int).Add(params.ExchangeRate,big.NewInt(int64(index)*100000000003))
  parent.Header().SetExchangeRate(rate)
  parent.Header().SetParentHash(grand.Hash(),common.PRIME_CTX)
  parent.SetParentHash(grand.Hash(),common.ZONE_CTX)
  parent.Header().SetConversionFlowAmount(new(big.Int).Mul(params.StartingConversionFlowAmount,big.NewInt(int64(index%5+1))))
  peBind(parent)
  oldRate:=new(big.Int).Mul(rate,big.NewInt(2));if index%2==0 {oldRate.Div(new(big.Int).Set(rate),big.NewInt(2))}
  old:=makeBlock(number-params.MinerDifficultyWindow,big.NewInt(1000000000001));old.Header().SetExchangeRate(oldRate);peBind(old)
  cache:=func(w *types.WorkObject) {rawdb.WriteHeaderNumber(db,w.Hash(),w.NumberU64(common.PRIME_CTX));rawdb.WriteTermini(db,w.Hash(),types.EmptyTermini());hc.bc.blockCache.Add(w.Hash(),*w)}
  cache(grand);cache(parent);cache(old);rawdb.WriteCanonicalHash(db,old.Hash(),old.NumberU64(common.PRIME_CTX))
  empty:=types.NewTokenChoiceSet();if e:=rawdb.WriteTokenChoicesSet(db,grand.Hash(),&empty);e!=nil {panic(e)}
  stored:=new(big.Int).Add(rate,big.NewInt(123456789));update:=uint8(1);if index%3==0 {update=0}
  hc.fetchKQuaiAndUpdateBit=func(hash common.Hash)(*big.Int,uint8,error){if hash!=parent.ParentHash(common.ZONE_CTX){return nil,0,fmt.Errorf("override branch key")};return new(big.Int).Set(stored),update,nil}
  qaiBytes:=make([]byte,20);qaiBytes[19]=1;qai:=common.BytesToAddress(qaiBytes,common.Location{0,0})
  qiBytes:=make([]byte,20);qiBytes[1]=128;qiBytes[19]=2;qi:=common.BytesToAddress(qiBytes,common.Location{0,0})
  txs:=types.Transactions{}
  for j:=0;j<24;j++ {
   recipient:=qai;if (j+index)%2==0 {recipient=qi}
   kind:=uint64(types.ConversionType);if j%7==0 {kind=types.CoinbaseType};if j%11==0 {kind=types.DefaultType}
   target:=new(big.Int).Div(new(big.Int).Mul(parent.ConversionFlowAmount(),big.NewInt(int64((j%7+1)*(index%13+1)))),big.NewInt(19))
   value:=new(big.Int).Set(target);if recipient.IsInQuaiLedgerScope(){value=misc.QuaiToQi(parent,rate,parent.MinerDifficulty(),target)}
   if (j+index)%13==0 {value.SetUint64(0)}
   slip:=[]uint16{0,1,29,30,31,5000,8999,9000,9001,65535}[(j+index)%10]
   data:=[]byte{byte(slip>>8),byte(slip)};if j%9==0 {data=[]byte{1}};if j%8==0 {data=[]byte{}}
   txs=append(txs,types.NewTx(&types.ExternalTx{OriginatingTxHash:common.Hash{30:byte(index),31:byte(j+1)},ETXIndex:uint16(j),Gas:100000,To:&recipient,Sender:qai,Value:value,Data:data,AccessList:types.AccessList{},EtxType:kind}))
  }
  if index%16==0 {txs=types.Transactions{}}
  if index%16==1 {for _,tx:=range txs {if types.IsConversionTx(tx){tx.SetValue(new(big.Int))}}}
  rawdb.WriteInboundEtxs(db,parent.Hash(),txs)
  child:=makeBlock(number+1,hc.ComputeMinerDifficulty(parent));child.Header().SetParentHash(parent.Hash(),common.PRIME_CTX)
  child.Header().SetExchangeRate(new(big.Int));child.Header().SetConversionFlowAmount(new(big.Int));child.Header().SetKQuaiDiscount(new(big.Int))
  diagnostics:=[]string{}
  accepted:=false
  for attempt:=0;attempt<4;attempt++ {
   e:=sl.verifyParentExchangeRateAndFlowAmount(child)
   if e==nil {accepted=true;break}
   message:=e.Error();diagnostics=append(diagnostics,message);match:=local.FindStringSubmatch(message)
   if len(match)!=2 {panic(fmt.Sprintf("case %d: %v",index,e))};amount,ok:=new(big.Int).SetString(match[1],10);if !ok {panic("expected decimal")}
   switch {
   case strings.HasPrefix(message,"invalid conversion flow amount used"):child.Header().SetConversionFlowAmount(amount)
   case strings.HasPrefix(message,"invalid exchange rate used"):child.Header().SetExchangeRate(amount)
   case strings.HasPrefix(message,"invalid newKQuaiDiscount used"):child.Header().SetKQuaiDiscount(amount)
   default:panic(message)
   }
  }
  if !accepted {panic("unmodified parent verifier did not accept corrected fields")}
  fields:=[]*big.Int{new(big.Int).Set(child.ConversionFlowAmount()),new(big.Int).Set(child.ExchangeRate()),new(big.Int).Set(child.KQuaiDiscount())}
  setters:=[]func(*big.Int){child.Header().SetConversionFlowAmount,child.Header().SetExchangeRate,child.Header().SetKQuaiDiscount}
  for i,set:=range setters {set(new(big.Int).Add(fields[i],big.NewInt(1)));if sl.verifyParentExchangeRateAndFlowAmount(child)==nil {panic("changed field accepted")};mutations++;set(fields[i])}
  rawdb.DeleteInboundEtxs(db,parent.Hash());if sl.verifyParentExchangeRateAndFlowAmount(child)==nil {panic("missing inbound accepted")};missing++;rawdb.WriteInboundEtxs(db,parent.Hash(),txs)
  rawdb.DeleteCanonicalHash(db,old.NumberU64(common.PRIME_CTX));if sl.verifyParentExchangeRateAndFlowAmount(child)==nil {panic("missing history accepted")};missing++;rawdb.WriteCanonicalHash(db,old.Hash(),old.NumberU64(common.PRIME_CTX))
  if e:=sl.verifyParentExchangeRateAndFlowAmount(child);e!=nil {panic(e)}
  child.SetParentHash(parent.Hash(),common.ZONE_CTX);child.SetNumber(new(big.Int).Add(parent.Number(common.ZONE_CTX),big.NewInt(1)),common.ZONE_CTX);child.WorkObjectHeader().SetTime(parent.Time()+5);peBind(child)
  seed:=map[string]interface{}{"grandparent":peWire(grand),"parent":peWire(parent),"grandparent_hash":grand.Hash().Hex(),"parent_hash":parent.Hash().Hex(),"inbound":pfEncode(txs),"old_exchange":oldRate.String(),"stored_exchange":stored.String(),"update_bit":update}
  type override struct {rate *big.Int;bit uint8}
  overrides:=map[common.Hash]override{parent.ParentHash(common.ZONE_CTX):{new(big.Int).Set(stored),update}}
  hc.fetchKQuaiAndUpdateBit=func(hash common.Hash)(*big.Int,uint8,error){v,ok:=overrides[hash];if !ok{return nil,0,fmt.Errorf("unavailable scoped override %s",hash)};return new(big.Int).Set(v.rate),v.bit,nil}
  if e:=sl.NativePrimeSaveParentChoicesProbe(child);e!=nil {panic(e)}
  if e:=sl.verifyParentExchangeRateAndFlowAmount(child);e!=nil {panic(e)}
  parentChoices:=rawdb.ReadTokenChoicesSet(db,parent.Hash());if parentChoices==nil {panic("seed parent choices")}
  seed["window_sha256"]=pfDigest(*parentChoices);seed["difficulty_sum"]=pfSum(*parentChoices)
  prototype:=pfClone(txs);steps:=[]interface{}{}
  var zeroProbe interface{}
  for step:=0;step<4;step++ {
   current:=child
   previous:=parent
   // Keep this source input immutable while the exact branch mutates its copy.
   raw:=pfClone(prototype)
   for _,tx:=range raw {if types.IsConversionTx(tx){v:=new(big.Int).Mul(tx.Value(),big.NewInt(int64(step+1)));if v.Sign()==0 {v.SetUint64(1)};tx.SetValue(v)}}
   rawdb.WriteInboundEtxs(db,current.Hash(),raw)
   oldCurrentRate:=new(big.Int).Mul(current.ExchangeRate(),big.NewInt(2));if (index+step)%2==0 {oldCurrentRate.Div(new(big.Int).Set(current.ExchangeRate()),big.NewInt(2))};if oldCurrentRate.Sign()==0 {oldCurrentRate.SetUint64(1)}
   historical:=makeBlock(current.NumberU64(common.PRIME_CTX)-params.MinerDifficultyWindow,big.NewInt(1000000000001));historical.Header().SetExchangeRate(oldCurrentRate);peBind(historical);cache(historical);rawdb.WriteCanonicalHash(db,historical.Hash(),historical.NumberU64(common.PRIME_CTX))
   forwardStored:=new(big.Int).Add(current.ExchangeRate(),big.NewInt(int64(333+step)));forwardUpdate:=uint8(1);if (index+step)%3==0 {forwardUpdate=0}
   overrides[current.ParentHash(common.ZONE_CTX)]=override{forwardStored,forwardUpdate}
   input:=pfEncode(raw)
   var pending ethdb.Batch
   output,e:=NativePrimeForwardEconomicBranch(sl,current,current,pfClone(raw),&pending);if e!=nil {panic(fmt.Errorf("case %d step %d: %w",index,step,e))}
   encodedOutput:=pfEncode(output)
   if e=pending.Write();e!=nil {panic(e)}
   choices:=rawdb.ReadTokenChoicesSet(db,current.Hash());if choices==nil {panic("current choice persistence")}
   var forwardRate *big.Int
   if previous.NumberU64(common.ZONE_CTX)<=params.BlocksPerYear && forwardUpdate==0 {forwardRate=new(big.Int).Set(forwardStored)} else {forwardRate,e=CalculateBetaFromMiningChoiceAndConversions(hc,previous,current.ExchangeRate(),*choices);if e!=nil {panic(e)}}
   refunds:=0;for _,tx:=range output {if tx.EtxType()==types.ConversionRevertType {refunds++}}
   steps=append(steps,map[string]interface{}{"step":step,"block":peWire(current),"block_hash":current.Hash().Hex(),"inbound":input,"old_exchange":oldCurrentRate.String(),"stored_exchange":forwardStored.String(),"update_bit":forwardUpdate,"output":encodedOutput,"output_root":types.DeriveSha(output,trie.NewStackTrie(nil)).Hex(),"refunds":refunds,"window_sha256":pfDigest(*choices),"difficulty_sum":pfSum(*choices),"forward_rate_reference":forwardRate.String(),"parent_verifier_accepted":true})
   totalSteps++
   if step==0 {
    zero:=pfClone(raw);found:=false
    for _,tx:=range zero {if types.IsConversionTx(tx){tx.SetValue(new(big.Int));found=true;break}}
    if found {
     observed,message:=pfObserveZeroPanic(sl,current,pfClone(zero))
     if !observed || !strings.Contains(message,"invalid memory address or nil pointer dereference") {panic(fmt.Sprintf("unexpected zero probe: %v %s",observed,message))}
     zeroProbe=map[string]interface{}{"inbound":pfEncode(zero),"panic_observed":observed,"panic_message":message,"batch_committed":false};nilProbes++
    }
   }
   cache(current)
   if step<3 {
    next:=makeBlock(current.NumberU64(common.PRIME_CTX)+1,hc.ComputeMinerDifficulty(current));next.SetParentHash(current.Hash(),common.PRIME_CTX);next.SetParentHash(current.Hash(),common.ZONE_CTX);next.SetNumber(new(big.Int).Add(current.Number(common.ZONE_CTX),big.NewInt(1)),common.ZONE_CTX);next.WorkObjectHeader().SetTime(current.Time()+5)
    next.Header().SetExchangeRate(new(big.Int));next.Header().SetConversionFlowAmount(new(big.Int));next.Header().SetKQuaiDiscount(new(big.Int))
    accepted:=false
    for attempt:=0;attempt<4;attempt++ {
     err:=sl.verifyParentExchangeRateAndFlowAmount(next);if err==nil {accepted=true;break};message:=err.Error();match:=local.FindStringSubmatch(message);if len(match)!=2 {panic(message)};amount,ok:=new(big.Int).SetString(match[1],10);if !ok {panic(message)}
     switch {case strings.HasPrefix(message,"invalid conversion flow amount used"):next.Header().SetConversionFlowAmount(amount);case strings.HasPrefix(message,"invalid exchange rate used"):next.Header().SetExchangeRate(amount);case strings.HasPrefix(message,"invalid newKQuaiDiscount used"):next.Header().SetKQuaiDiscount(amount);default:panic(message)}
    }
    if !accepted {panic("linked parent verifier did not converge")};peBind(next)
    child=next;parent=current
   }
  }
  vectors=append(vectors,map[string]interface{}{"case":index,"seed":seed,"steps":steps,"undefined_zero_probe":zeroProbe})
  db.Close()
 }
 return map[string]interface{}{"vectors":vectors,"go_field_mutations_rejected":mutations,"go_missing_history_rejected":missing,"forward_steps":totalSteps,"undefined_zero_probe_cases":nilProbes}
}
