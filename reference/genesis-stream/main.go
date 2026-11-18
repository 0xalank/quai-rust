package main
import ("os";"github.com/dominant-strategies/go-quai/log";"strconv";"fmt";"github.com/dominant-strategies/go-quai/core")
var referenceRevision string
func main(){log.Global.SetOutput(os.Stderr);if referenceRevision!="33db82f2891b267ba898ca38bd2dd8bf75cbb072"{panic("reference pin")};if len(os.Args)!=2{panic("expected block count")};n,e:=strconv.ParseUint(os.Args[1],10,64);if e!=nil||n==0||n>300000{panic(fmt.Sprint("block bound ",e))};core.NativeGenesisStream(n,os.Stdout)}
