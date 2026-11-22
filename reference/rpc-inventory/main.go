// RPC inventory oracle: every JSON-RPC method and subscription that go-quai
// v0.57.0 registers for a zone node, and which of them a node with the
// default flags serves over HTTP and over WebSocket.
//
// The API list comes from the source: the rpc.API literals of
// quaiapi.GetAPIs (internal/quaiapi/backend.go) and Quai.APIs
// (quai/backend.go), parsed with go/parser, in registration order, with the
// zone-only block of GetAPIs. Each service is a zero value of its
// constructor's result type: registration (rpc/service.go) reads only the
// type's methods. The configuration is makeConfigNode's (cmd/utils/cmd.go:
// defaultNodeConfig, then SetNodeConfig over the start command's flags, none
// set, no config file, no environment). The node is the production node.Node
// with that configuration, a temporary data directory and two free loopback
// ports. Each method is probed with params "probe": rpc.Server answers
// -32602 ("non-array args") for a registered method before it runs the
// handler, and -32601 for an unknown one. A subscription is probed with its
// name and too many arguments. No handler body runs; nothing leaves the
// loopback interface.
package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/dominant-strategies/go-quai/cmd/utils"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/internal/quaiapi"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/node"
	"github.com/dominant-strategies/go-quai/quai"
	"github.com/dominant-strategies/go-quai/quai/filters"
	"github.com/dominant-strategies/go-quai/rpc"
	"github.com/gorilla/websocket"
	"github.com/spf13/cobra"
)

var referenceRevision string

type obj = map[string]interface{}

func must(err error) {
	if err != nil {
		panic(err)
	}
}

// serviceTypes maps a constructor's result type to a zero service value.
var serviceTypes = map[string]func() interface{}{
	"quaiapi.PublicQuaiAPI":            func() interface{} { return new(quaiapi.PublicQuaiAPI) },
	"quaiapi.PublicBlockChainAPI":      func() interface{} { return new(quaiapi.PublicBlockChainAPI) },
	"quaiapi.PublicBlockChainQuaiAPI":  func() interface{} { return new(quaiapi.PublicBlockChainQuaiAPI) },
	"quaiapi.PublicDebugAPI":           func() interface{} { return new(quaiapi.PublicDebugAPI) },
	"quaiapi.PrivateDebugAPI":          func() interface{} { return new(quaiapi.PrivateDebugAPI) },
	"quaiapi.PublicNetAPI":             func() interface{} { return new(quaiapi.PublicNetAPI) },
	"quaiapi.PublicTransactionPoolAPI": func() interface{} { return new(quaiapi.PublicTransactionPoolAPI) },
	"quaiapi.PublicTxPoolAPI":          func() interface{} { return new(quaiapi.PublicTxPoolAPI) },
	"quaiapi.PublicWorkSharesAPI":      func() interface{} { return new(quaiapi.PublicWorkSharesAPI) },
	"quai.PublicQuaiAPI":               func() interface{} { return new(quai.PublicQuaiAPI) },
	"quai.PublicMinerAPI":              func() interface{} { return new(quai.PublicMinerAPI) },
	"quai.PrivateMinerAPI":             func() interface{} { return new(quai.PrivateMinerAPI) },
	"quai.PrivateAdminAPI":             func() interface{} { return new(quai.PrivateAdminAPI) },
	"quai.PublicDebugAPI":              func() interface{} { return new(quai.PublicDebugAPI) },
	"quai.PrivateDebugAPI":             func() interface{} { return new(quai.PrivateDebugAPI) },
	"filters.PublicFilterAPI":          func() interface{} { return new(filters.PublicFilterAPI) },
}

// packageDirs maps a package name used in the two API files to its directory.
var packageDirs = map[string]string{
	"quaiapi": "internal/quaiapi",
	"quai":    "quai",
	"filters": "quai/filters",
}

type apiEntry struct {
	Namespace   string `json:"namespace"`
	Version     string `json:"version"`
	Constructor string `json:"constructor"`
	Type        string `json:"type"`
	Public      bool   `json:"public"`
	ZoneOnly    bool   `json:"zone_only"`
	Source      string `json:"source"`
}

// resultType finds the constructor in its package and returns "pkg.Type"
// of its single *Type result.
func resultType(root, pkg, name string) string {
	dir := filepath.Join(root, packageDirs[pkg])
	fset := token.NewFileSet()
	pkgs, err := parser.ParseDir(fset, dir, func(fi os.FileInfo) bool {
		return !strings.HasSuffix(fi.Name(), "_test.go")
	}, 0)
	must(err)
	found := ""
	for _, p := range pkgs {
		for _, f := range p.Files {
			for _, d := range f.Decls {
				fn, ok := d.(*ast.FuncDecl)
				if !ok || fn.Recv != nil || fn.Name.Name != name {
					continue
				}
				if fn.Type.Results == nil || len(fn.Type.Results.List) != 1 {
					panic("constructor " + pkg + "." + name + " has no single result")
				}
				star, ok := fn.Type.Results.List[0].Type.(*ast.StarExpr)
				if !ok {
					panic("constructor " + pkg + "." + name + " does not return a pointer")
				}
				ident, ok := star.X.(*ast.Ident)
				if !ok {
					panic("constructor " + pkg + "." + name + " returns a qualified type")
				}
				found = pkg + "." + ident.Name
			}
		}
	}
	if found == "" {
		panic("constructor " + pkg + "." + name + " not found")
	}
	return found
}

// parseAPIs reads the rpc.API literals of one function in source order.
func parseAPIs(root, file, function, pkg string) []apiEntry {
	fset := token.NewFileSet()
	f, err := parser.ParseFile(fset, filepath.Join(root, file), nil, 0)
	must(err)
	var body *ast.BlockStmt
	for _, d := range f.Decls {
		if fn, ok := d.(*ast.FuncDecl); ok && fn.Name.Name == function {
			body = fn.Body
		}
	}
	if body == nil {
		panic("function " + function + " not found in " + file)
	}
	type span struct{ from, to token.Pos }
	zone := []span{}
	ast.Inspect(body, func(n ast.Node) bool {
		if s, ok := n.(*ast.IfStmt); ok {
			cond, ok := s.Cond.(*ast.BinaryExpr)
			if !ok || cond.Op != token.EQL {
				panic("unexpected condition in " + function)
			}
			l, lok := cond.X.(*ast.Ident)
			r, rok := cond.Y.(*ast.SelectorExpr)
			if !lok || !rok || l.Name != "nodeCtx" || r.Sel.Name != "ZONE_CTX" {
				panic("unexpected condition in " + function)
			}
			zone = append(zone, span{s.Body.Pos(), s.Body.End()})
		}
		return true
	})
	entries := []apiEntry{}
	ast.Inspect(body, func(n ast.Node) bool {
		lit, ok := n.(*ast.CompositeLit)
		if !ok {
			return true
		}
		fields := map[string]ast.Expr{}
		for _, e := range lit.Elts {
			if kv, ok := e.(*ast.KeyValueExpr); ok {
				if k, ok := kv.Key.(*ast.Ident); ok {
					fields[k.Name] = kv.Value
				}
			}
		}
		if _, ok := fields["Namespace"]; !ok {
			return true
		}
		e := apiEntry{Source: fmt.Sprintf("%s:%d", file, fset.Position(lit.Pos()).Line)}
		ns, err := strconv.Unquote(fields["Namespace"].(*ast.BasicLit).Value)
		must(err)
		e.Namespace = ns
		v, err := strconv.Unquote(fields["Version"].(*ast.BasicLit).Value)
		must(err)
		e.Version = v
		call := fields["Service"].(*ast.CallExpr)
		switch fun := call.Fun.(type) {
		case *ast.Ident:
			e.Constructor = pkg + "." + fun.Name
			e.Type = resultType(root, pkg, fun.Name)
		case *ast.SelectorExpr:
			p := fun.X.(*ast.Ident).Name
			e.Constructor = p + "." + fun.Sel.Name
			e.Type = resultType(root, p, fun.Sel.Name)
		default:
			panic("unexpected service expression in " + function)
		}
		if p, ok := fields["Public"]; ok {
			e.Public = p.(*ast.Ident).Name == "true"
		}
		for _, s := range zone {
			if lit.Pos() >= s.from && lit.End() <= s.to {
				e.ZoneOnly = true
			}
		}
		for k := range fields {
			if k != "Namespace" && k != "Version" && k != "Service" && k != "Public" {
				panic("unexpected rpc.API field " + k)
			}
		}
		entries = append(entries, e)
		return false
	})
	return entries
}

// receiverType turns a runtime function name into "pkg.Type".
func receiverType(function string) string {
	// github.com/dominant-strategies/go-quai/internal/quaiapi.(*PublicQuaiAPI).GasPrice
	slash := strings.LastIndex(function, "/")
	rest := function[slash+1:]
	dot := strings.Index(rest, ".")
	pkg := rest[:dot]
	recv := rest[dot+1:]
	recv = recv[:strings.LastIndex(recv, ".")]
	recv = strings.TrimSuffix(strings.TrimPrefix(recv, "(*"), ")")
	return pkg + "." + recv
}

// declarations maps "file:Type.Method" to the line of the func keyword.
var declarations = map[string]int{}

// declarationLine finds the method declaration in its file: the runtime's
// entry line of a small function can be its first statement instead.
func declarationLine(root, file, function string) int {
	name := function[strings.LastIndex(function, ".")+1:]
	recv := receiverType(function)
	recv = recv[strings.Index(recv, ".")+1:]
	key := file + ":" + recv + "." + name
	if line, ok := declarations[key]; ok {
		return line
	}
	fset := token.NewFileSet()
	f, err := parser.ParseFile(fset, filepath.Join(root, file), nil, 0)
	must(err)
	for _, d := range f.Decls {
		fn, ok := d.(*ast.FuncDecl)
		if !ok || fn.Recv == nil || len(fn.Recv.List) != 1 {
			continue
		}
		t := fn.Recv.List[0].Type
		if s, ok := t.(*ast.StarExpr); ok {
			t = s.X
		}
		if id, ok := t.(*ast.Ident); ok {
			declarations[file+":"+id.Name+"."+fn.Name.Name] = fset.Position(fn.Pos()).Line
		}
	}
	line, ok := declarations[key]
	if !ok {
		panic("no declaration of " + key)
	}
	return line
}

func freePort() int {
	l, err := net.Listen("tcp", "127.0.0.1:0")
	must(err)
	defer l.Close()
	return l.Addr().(*net.TCPAddr).Port
}

type answer struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
	Result  json.RawMessage
}

func decodeAnswer(raw []byte) answer {
	var m struct {
		Result json.RawMessage `json:"result"`
		Error  *struct {
			Code    int    `json:"code"`
			Message string `json:"message"`
		} `json:"error"`
	}
	must(json.Unmarshal(raw, &m))
	if m.Error != nil {
		return answer{Code: m.Error.Code, Message: m.Error.Message}
	}
	return answer{Result: m.Result}
}

func request(method string, params interface{}) []byte {
	body, err := json.Marshal(obj{"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
	must(err)
	return body
}

func askHTTP(url, method string, params interface{}) answer {
	resp, err := http.Post(url, "application/json", bytes.NewReader(request(method, params)))
	must(err)
	defer resp.Body.Close()
	raw, err := io.ReadAll(resp.Body)
	must(err)
	if resp.StatusCode != http.StatusOK {
		panic(fmt.Sprintf("HTTP %d for %s: %s", resp.StatusCode, method, raw))
	}
	return decodeAnswer(raw)
}

func askWS(conn *websocket.Conn, method string, params interface{}) answer {
	must(conn.SetWriteDeadline(time.Now().Add(10 * time.Second)))
	must(conn.SetReadDeadline(time.Now().Add(10 * time.Second)))
	must(conn.WriteMessage(websocket.TextMessage, request(method, params)))
	_, raw, err := conn.ReadMessage()
	must(err)
	return decodeAnswer(raw)
}

// served reads a probe answer: -32602 is a registered method or
// subscription, -32601 an unknown one.
func served(a answer) bool {
	switch a.Code {
	case -32602:
		return true
	case -32601:
		return false
	}
	panic(fmt.Sprintf("unexpected probe answer %d %q", a.Code, a.Message))
}

func outcome(a answer) obj {
	return obj{"code": a.Code, "message": a.Message}
}

// Names outside every registry that wallets and Ethereum tools call.
var outsideNames = []string{
	"web3_clientVersion", "web3_sha3",
	"eth_sendTransaction", "eth_accounts", "eth_requestAccounts", "eth_sign",
	"eth_signTransaction", "eth_signTypedData_v4", "personal_sign",
	"personal_listAccounts", "personal_unlockAccount", "personal_sendTransaction",
	"eth_syncing", "quai_syncing", "eth_feeHistory", "quai_feeHistory",
	"eth_maxPriorityFeePerGas", "quai_maxPriorityFeePerGas", "eth_getBlockReceipts",
	"quai_getBlockReceipts", "eth_protocolVersion", "quai_protocolVersion",
	"quai_accounts", "quai_requestAccounts", "quai_sendTransaction", "quai_sign",
	"quai_signTransaction", "quai_mining", "quai_hashrate",
	"eth_hashrate", "eth_coinbase", "admin_nodeInfo", "admin_peers", "miner_start",
	"txpool_getRollingFeeInfoV2", "quai_getRollingFeeInfo",
}

func main() {
	root, err := filepath.Abs(os.Args[1])
	must(err)
	log.SetGlobalLogger("", "fatal")

	// API list from the source.
	apis := parseAPIs(root, "internal/quaiapi/backend.go", "GetAPIs", "quaiapi")
	apis = append(apis, parseAPIs(root, "quai/backend.go", "APIs", "quai")...)
	list := []rpc.API{}
	for _, e := range apis {
		newService, ok := serviceTypes[e.Type]
		if !ok {
			panic("no service type for " + e.Type)
		}
		list = append(list, rpc.API{Namespace: e.Namespace, Version: e.Version, Service: newService(), Public: e.Public})
	}

	// Every registered method and subscription.
	full := rpc.NewServer(log.Global)
	must(node.RegisterApis(list, nil, full, true, log.Global))
	callbacks := full.InventoryCallbacks()

	// The default configuration of a zone-0-0 node.
	cmd := &cobra.Command{Use: "start"}
	for _, group := range utils.Flags {
		for _, f := range group {
			utils.CreateAndBindFlag(f, cmd)
		}
	}
	must(cmd.PersistentFlags().Parse([]string{}))
	location := common.Location{0, 0}
	cfg := utils.RPCInventoryNodeConfig(location)
	config := obj{
		"http": obj{"host": cfg.HTTPHost, "port": cfg.HTTPPort, "modules": cfg.HTTPModules,
			"vhosts": cfg.HTTPVirtualHosts, "cors": cfg.HTTPCors, "prefix": cfg.HTTPPathPrefix},
		"ws": obj{"host": cfg.WSHost, "port": cfg.WSPort, "modules": cfg.WSModules,
			"origins": cfg.WSOrigins, "prefix": cfg.WSPathPrefix},
	}
	if cfg.HTTPHost == "" || cfg.WSHost == "" {
		panic("a default endpoint is off")
	}

	// The production node with that configuration on free loopback ports.
	dir, err := os.MkdirTemp(".", "node-")
	must(err)
	defer os.RemoveAll(dir)
	run := cfg
	run.DataDir, err = filepath.Abs(dir)
	must(err)
	run.HTTPPort = freePort()
	for run.WSPort = freePort(); run.WSPort == run.HTTPPort; run.WSPort = freePort() {
	}
	stack, err := node.New(&run, log.Global)
	must(err)
	stack.RegisterAPIs(list)
	must(stack.Start())
	defer stack.Close()
	httpURL := stack.HTTPEndpoint()
	wsURL := stack.WSEndpoint()
	conn, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	must(err)
	defer conn.Close()

	modules := obj{}
	for name, a := range map[string]answer{
		"http": askHTTP(httpURL, "rpc_modules", []interface{}{}),
		"ws":   askWS(conn, "rpc_modules", []interface{}{}),
	} {
		var m map[string]string
		must(json.Unmarshal(a.Result, &m))
		modules[name] = m
	}

	// Probe every registered name over both transports.
	byType := map[string][]apiEntry{}
	for _, e := range apis {
		byType[e.Namespace+"/"+e.Type] = append(byType[e.Namespace+"/"+e.Type], e)
	}
	methods := []interface{}{}
	for _, cb := range callbacks {
		file := strings.TrimPrefix(cb.File, root+string(filepath.Separator))
		if strings.HasPrefix(file, "<") {
			panic("generated wrapper for " + cb.Function)
		}
		entries := byType[cb.Namespace+"/"+receiverType(cb.Function)]
		if cb.Namespace != "rpc" && len(entries) == 0 {
			panic("no API entry for " + cb.Function)
		}
		public, zoneOnly, sources := cb.Namespace == "rpc", false, []string{}
		for _, e := range entries {
			public = public || e.Public
			zoneOnly = e.ZoneOnly
			sources = append(sources, e.Source)
		}
		var h, w answer
		name := cb.Namespace + "_" + cb.Name
		if cb.Subscription {
			params := []interface{}{cb.Name, 1, 2, 3, 4, 5, 6, 7, 8}
			h = askHTTP(httpURL, cb.Namespace+"_subscribe", params)
			w = askWS(conn, cb.Namespace+"_subscribe", params)
		} else {
			h = askHTTP(httpURL, name, "probe")
			w = askWS(conn, name, "probe")
		}
		m := obj{
			"namespace": cb.Namespace, "name": cb.Name, "method": name,
			"kind": "method", "function": cb.Function,
			"source": fmt.Sprintf("%s:%d", file, declarationLine(root, file, cb.Function)),
			"args":   cb.Args, "results": cb.Results,
			"public": public, "zone_only": zoneOnly, "registered_by": sources,
			"ws": obj{"served": served(w), "probe": outcome(w)},
		}
		if cb.Subscription {
			m["kind"] = "subscription"
			m["method"] = cb.Namespace + "_subscribe"
			// rpc.Server refuses every *_subscribe over HTTP before it
			// looks at the registry (rpc/server.go serveSingleRequest).
			m["http"] = obj{"served": false, "probe": outcome(h)}
		} else {
			m["http"] = obj{"served": served(h), "probe": outcome(h)}
		}
		methods = append(methods, m)
	}

	outside := []interface{}{}
	for _, name := range outsideNames {
		for _, cb := range callbacks {
			if !cb.Subscription && cb.Namespace+"_"+cb.Name == name {
				panic(name + " is registered")
			}
		}
		h, w := askHTTP(httpURL, name, "probe"), askWS(conn, name, "probe")
		outside = append(outside, obj{"method": name,
			"http": obj{"served": served(h), "probe": outcome(h)},
			"ws":   obj{"served": served(w), "probe": outcome(w)}})
	}
	// *_unsubscribe goes to one callback for every namespace (rpc/handler.go).
	unsub := []interface{}{}
	for _, name := range []string{"quai_unsubscribe", "eth_unsubscribe", "nosuch_unsubscribe"} {
		h, w := askHTTP(httpURL, name, "probe"), askWS(conn, name, "probe")
		unsub = append(unsub, obj{"method": name, "http": outcome(h), "ws": outcome(w)})
	}

	sort.SliceStable(methods, func(i, j int) bool {
		a, b := methods[i].(obj), methods[j].(obj)
		if a["namespace"] != b["namespace"] {
			return a["namespace"].(string) < b["namespace"].(string)
		}
		if a["kind"] != b["kind"] {
			return a["kind"].(string) < b["kind"].(string)
		}
		return a["name"].(string) < b["name"].(string)
	})
	out := obj{
		"reference_revision": referenceRevision,
		"scope": "go-quai v0.57.0 JSON-RPC registry of a zone node (quaiapi.GetAPIs and " +
			"Quai.APIs), probed on the production node.Node built from makeConfigNode's " +
			"default configuration for zone [0 0]; no handler body runs",
		"location":    location,
		"config":      config,
		"apis":        apis,
		"rpc_modules": modules,
		"methods":     methods,
		"outside":     outside,
		"unsubscribe": unsub,
	}
	encoded, err := json.MarshalIndent(out, "", " ")
	must(err)
	os.Stdout.Write(append(encoded, '\n'))
}
