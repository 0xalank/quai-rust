// Observation-only adapter for the RPC inventory oracle
// (reference/rpc-inventory): lists the methods and subscriptions that a
// Server's registry holds, with the Go function that serves each. It reads
// the registry that RegisterName (rpc/service.go) fills; it changes nothing.
package rpc

import (
	"runtime"
	"sort"
)

// InventoryCallback is one registered method or subscription.
type InventoryCallback struct {
	Namespace    string
	Name         string
	Subscription bool
	Function     string
	File         string
	Line         int
	Args         []string
	Results      []string
}

// InventoryCallbacks lists the registry in namespace and name order.
func (s *Server) InventoryCallbacks() []InventoryCallback {
	s.services.mu.Lock()
	defer s.services.mu.Unlock()
	out := []InventoryCallback{}
	add := func(namespace, name string, subscription bool, cb *callback) {
		f := runtime.FuncForPC(cb.fn.Pointer())
		file, line := f.FileLine(f.Entry())
		args := []string{}
		for _, t := range cb.argTypes {
			args = append(args, t.String())
		}
		results := []string{}
		ft := cb.fn.Type()
		for i := 0; i < ft.NumOut(); i++ {
			results = append(results, ft.Out(i).String())
		}
		out = append(out, InventoryCallback{namespace, name, subscription, f.Name(), file, line, args, results})
	}
	for namespace, svc := range s.services.services {
		for name, cb := range svc.callbacks {
			add(namespace, name, false, cb)
		}
		for name, cb := range svc.subscriptions {
			add(namespace, name, true, cb)
		}
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].Namespace != out[j].Namespace {
			return out[i].Namespace < out[j].Namespace
		}
		if out[i].Subscription != out[j].Subscription {
			return !out[i].Subscription
		}
		return out[i].Name < out[j].Name
	})
	return out
}
