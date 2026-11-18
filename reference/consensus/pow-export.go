// Offline oracle access to the actual package-private light kernel. Not shipped.
package POWPACKAGE
import (
 "encoding/binary"
 "math/big"
 "github.com/dominant-strategies/go-quai/core/types"
 "github.com/dominant-strategies/go-quai/common"
 "google.golang.org/protobuf/proto"
 "github.com/dominant-strategies/go-quai/crypto"
 "github.com/dominant-strategies/go-quai/log"
 "golang.org/x/crypto/sha3"
)
func NativeConsensusVector(height uint64,input []byte,nonce uint64) map[string]interface{} {
 cache:=make([]uint32,cacheSize(height)/4);epoch:=height/C_epochLength
 generateCache(cache,epoch,seedHash(height),log.Global)
 cdag:=make([]uint32,4096);generateCDag(cdag,cache,epoch,log.Global)
 cases:=[]interface{}{}
 for _,h:=range []uint64{height,height+1,height+2,height+3,height+9,height+10,height+11} {
  if h/C_epochLength!=epoch {continue}
  n:=nonce^(h-height);data:=append([]byte{},input...)
  if "POWPACKAGE"=="kawpow" {for i,j:=0,len(data)-1;i<j;i,j=i+1,j-1 {data[i],data[j]=data[j],data[i]}}
  digest,result:=POWKERNEL(datasetSize(h),cache,data,n,h,cdag)
  if "POWPACKAGE"=="kawpow" {for i,j:=0,len(digest)-1;i<j;i,j=i+1,j-1 {digest[i],digest[j]=digest[j],digest[i]}}
  cases=append(cases,map[string]interface{}{"height":h,"nonce":n,"seal":hxNative(input),"mix":hxNative(digest),"hash":hxNative(result)})
 }

 objects:=[]interface{}{}
 if "POWPACKAGE"=="progpow" {for _,h:=range []uint64{height,height+10} {
  wo:=types.EmptyWorkObject(common.ZONE_CTX);wh:=wo.WorkObjectHeader();wh.SetLocation(common.Location{0,0});wh.SetPrimaryCoinbase(common.BytesToAddress(make([]byte,20),common.Location{0,0}));wh.SetDifficulty(big.NewInt(2));wh.SetTime(12345);wh.SetNumber(big.NewInt(1));wh.SetPrimeTerminusNumber(new(big.Int).SetUint64(h));wh.SetData([]byte{0});wh.SetHeaderHash(wo.Header().Hash())
  seal:=wh.SealHash();var digest,result []byte;var n uint64
  for {digest,result=POWKERNEL(datasetSize(h),cache,seal.Bytes(),n,h,cdag);if new(big.Int).SetBytes(result).Cmp(new(big.Int).Rsh(new(big.Int).Lsh(big.NewInt(1),256),1))<=0 {break};n++}
  wh.SetNonce(types.EncodeNonce(n));wh.SetMixHash(common.BytesToHash(digest));wire,e:=wh.ProtoEncode();if e!=nil {panic(e)};header,e:=proto.Marshal(wire);if e!=nil {panic(e)};roundTrip:=new(types.WorkObjectHeader);if e=roundTrip.ProtoDecode(wire,common.Location{0,0});e!=nil {panic(e)};if roundTrip.SealHash()!=seal {panic("noncanonical oracle header seal")};if roundTrip.NonceU64()!=n {panic("noncanonical oracle nonce")}
  full,e:=wo.ProtoEncode(types.BlockObject);if e!=nil {panic(e)};object,e:=proto.Marshal(full);if e!=nil {panic(e)}
  bad:=proto.Clone(wire).(*types.ProtoWorkObjectHeader);bad.MixHash.Value[0]^=1;badBytes,e:=proto.Marshal(bad);if e!=nil {panic(e)}
  objects=append(objects,map[string]interface{}{"header":hxNative(header),"object":hxNative(object),"mutated_mix_header":hxNative(badBytes),"pow":hxNative(result),"seal":seal.Hex(),"height":h,"nonce":n,"body_header_hash":wo.Header().Hash().Hex()})
 }}
 raw:=make([]byte,len(cache)*4);for i,v:=range cache {binary.LittleEndian.PutUint32(raw[4*i:],v)}
 small:=make([]byte,len(cdag)*4);for i,v:=range cdag {binary.LittleEndian.PutUint32(small[4*i:],v)}
 items:=[]interface{}{};for _,i:=range []uint32{0,1,255,256,65535,0x1fffff} {v:=generateDatasetItem(cache,i,makeHasher(sha3.NewLegacyKeccak512()));items=append(items,map[string]interface{}{"index":i,"value":hxNative(v)})}
 return map[string]interface{}{"algorithm":"POWPACKAGE","height":height,"cache_bytes":len(raw),"dataset_size":datasetSize(height),"cache_digest":crypto.Keccak256Hash(raw).Hex(),"cdag_digest":crypto.Keccak256Hash(small).Hex(),"seed":hxNative(seedHash(height)),"vectors":cases,"items":items,"objects":objects}
}
func hxNative(b []byte)string {const digits="0123456789abcdef";s:=make([]byte,2+len(b)*2);copy(s,"0x");for i,c:=range b {s[2+i*2]=digits[c>>4];s[3+i*2]=digits[c&15]};return string(s)}
