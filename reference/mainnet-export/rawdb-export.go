// Offline-only receipt lookup for the QEX1 mainnet exporter
// (reference/mainnet-export/main.go). The build adds this file to package
// rawdb with `go build -overlay`; the pinned go-quai checkout is not modified.
//
// It reads the same stores in the same order as ReadReceiptsProto, but it
// also reports whether go-quai stored an entry at all. An empty entry is
// valid (genesis, and every block without transactions); a missing entry is
// not, and the exporter stops on it.
package rawdb

import (
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/ethdb"
)

// NativeExportReceiptsKey returns the key-value key of a block's receipts:
// "r" + number (uint64 big endian) + hash.
func NativeExportReceiptsKey(number uint64, hash common.Hash) []byte {
	return blockReceiptsKey(number, hash)
}

// NativeExportReceipts returns the stored receipts value of a block (the
// bytes WriteReceipts stores: proto.Marshal of a ProtoReceiptsForStorage) and
// whether an entry exists. The ancient store is checked first, as in
// ReadReceiptsProto, then the key-value store.
func NativeExportReceipts(db ethdb.Reader, hash common.Hash, number uint64) ([]byte, bool, error) {
	if frozen, err := db.Ancient(freezerHashTable, number); err == nil && common.BytesToHash(frozen) == hash {
		data, err := db.Ancient(freezerReceiptTable, number)
		if err != nil {
			return nil, false, err
		}
		return data, true, nil
	}
	key := blockReceiptsKey(number, hash)
	found, err := db.Has(key)
	if err != nil || !found {
		return nil, false, err
	}
	data, err := db.Get(key)
	if err != nil {
		return nil, false, err
	}
	return data, true, nil
}
