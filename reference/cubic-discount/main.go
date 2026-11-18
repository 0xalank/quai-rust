// Explicit offline oracle for unmodified official conversion discount arithmetic.
package main
import (
 "crypto/sha256"
 "encoding/json"
 "fmt"
 "math/big"
 "github.com/dominant-strategies/go-quai/consensus/misc"
)
var referenceRevision string
func main() {
 maximum := new(big.Int).Sub(new(big.Int).Lsh(big.NewInt(1),256),big.NewInt(1))
 bound := func(n *big.Int) *big.Int {
  if n.Sign()<0 { return new(big.Int) }
  if n.Cmp(maximum)>0 { return new(big.Int).Set(maximum) }
  return n
 }
 random := func(label string, i int) *big.Int {
  bytes:=sha256.Sum256([]byte(fmt.Sprintf("prime-cubic-v1:%s:%d",label,i)))
  n:=new(big.Int).SetBytes(bytes[:])
  return n.Rsh(n,uint(256-(i%257)))
 }
 vectors:=[]interface{}{}
 for i:=0;i<4096;i++ {
  mean:=random("mean",i)
  value:=random("value",(i*197+17)%4096)
  switch i%16 {
   case 1: value.SetUint64(0)
   case 2: value.Set(mean)
   case 3: value.Add(mean,big.NewInt(1))
   case 4: value.Sub(mean,big.NewInt(1))
   case 5: value.Mul(mean,big.NewInt(2))
   case 6: value.Sub(new(big.Int).Mul(mean,big.NewInt(10)),big.NewInt(1))
   case 7: value.Mul(mean,big.NewInt(10))
   case 8: value.Add(new(big.Int).Mul(mean,big.NewInt(10)),big.NewInt(1))
   case 9: value.Set(maximum);mean.Sub(maximum,big.NewInt(int64(i)))
   case 10: value.SetUint64(uint64(i%97));mean.Set(maximum)
   case 11: mean.SetUint64(1);value.SetUint64(uint64(i%15))
   case 12: mean.Lsh(big.NewInt(1),uint(64+i%192));value.Add(mean,big.NewInt(int64(i%67)))
   case 13: value.Div(new(big.Int).Mul(mean,big.NewInt(998)),big.NewInt(1000))
   case 14: value.Div(new(big.Int).Mul(mean,big.NewInt(1001)),big.NewInt(1000))
   case 15: value.Div(new(big.Int).Mul(mean,big.NewInt(9990)),big.NewInt(1000))
  }
  value=bound(value);mean=bound(mean)
  result:=misc.ApplyCubicDiscount(value,mean)
  expected,_:=result.Int(nil)
  vectors=append(vectors,map[string]interface{}{"case":i,"family":i%16,"value":value.String(),"mean":mean.String(),"expected":expected.String(),"result_precision":result.Prec()})
 }
 encoded,err:=json.Marshal(map[string]interface{}{"reference_revision":referenceRevision,"scope":"Controlled uint256 inputs to official misc.ApplyCubicDiscount with exact Go binary rounding and final integer truncation; no ancestry or canonical authority.","vectors":vectors})
 if err!=nil { panic(err) };fmt.Println(string(encoded))
}
