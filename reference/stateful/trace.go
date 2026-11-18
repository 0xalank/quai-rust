package main

import (
	"encoding/binary"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core/vm"
	"github.com/dominant-strategies/go-quai/crypto"
	"math/big"
	"time"
)

type trace struct {
	hash    common.Hash
	steps   uint64
	opcodes map[byte]bool
}

func (t *trace) CaptureStart(*vm.EVM, common.Address, common.Address, bool, []byte, uint64, *big.Int) {
}
func (t *trace) CaptureEnd([]byte, uint64, time.Duration, error) {}
func (t *trace) CaptureFault(*vm.EVM, uint64, vm.OpCode, uint64, uint64, *vm.ScopeContext, int, error) {
}
func (t *trace) CaptureState(env *vm.EVM, pc uint64, op vm.OpCode, gas, cost uint64, scope *vm.ScopeContext, _ []byte, _ int, err error, _ common.Location) {
	if err != nil {
		return
	}
	data := append([]byte{}, t.hash.Bytes()...)
	add := func(n uint64) { var b [8]byte; binary.BigEndian.PutUint64(b[:], n); data = append(data, b[:]...) }
	add(pc)
	data = append(data, byte(op))
	add(gas)
	add(scope.Contract.StateGas)
	add(env.StateDB.GetRefund())
	add(uint64(len(scope.Stack.Data())))
	for _, value := range scope.Stack.Data() {
		b := value.Bytes32()
		data = append(data, b[:]...)
	}
	add(uint64(scope.Memory.Len()))
	data = append(data, crypto.Keccak256(scope.Memory.Data())...)
	t.hash = crypto.Keccak256Hash(data)
	t.steps++
	if t.opcodes == nil {
		t.opcodes = map[byte]bool{}
	}
	t.opcodes[byte(op)] = true
}
