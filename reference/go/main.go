// The reference server and load generator use the adjacent go-quai RPC package.
// Fixture methods perform no chain, database, EVM or consensus work.
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"math"
	"net"
	"net/http"
	"os"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/rpc"
)

var payloadData = strings.Repeat("0123456789abcdef", 64)
var payloadHash = "0x" + strings.Repeat("ab", 32)

type Payload struct {
	Sequence uint64 `json:"sequence"`
	Hash     string `json:"hash"`
	Data     string `json:"data"`
}
type BenchService struct{}

func (*BenchService) BlockNumber() string         { return "0xa7d8c0" }
func (*BenchService) Echo(sequence uint64) uint64 { return sequence }
func (*BenchService) Payload(sequence uint64) Payload {
	return Payload{sequence, payloadHash, payloadData}
}
func (*BenchService) Null() interface{}        { return nil }
func (*BenchService) Failure() (string, error) { return "", codedError{-32042, "benchmark failure"} }
func (*BenchService) Delay(ctx context.Context, ms uint64) (uint64, error) {
	if ms > 1000 {
		return 0, codedError{-32602, "delay exceeds fixture limit"}
	}
	timer := time.NewTimer(time.Duration(ms) * time.Millisecond)
	defer timer.Stop()
	select {
	case <-timer.C:
		return ms, nil
	case <-ctx.Done():
		return 0, ctx.Err()
	}
}

type QuaiService struct{}

func (*QuaiService) ChainId() string                         { return "0x9" }
func (*QuaiService) BlockNumber() string                     { return "0xa7d8c0" }
func (*QuaiService) GetBalance(address, block string) string { return "0x2a" }

type codedError struct {
	code    int
	message string
}

func (e codedError) Error() string  { return e.message }
func (e codedError) ErrorCode() int { return e.code }

func serve(args []string) error {
	flags := flag.NewFlagSet("serve", flag.ContinueOnError)
	listen := flags.String("listen", "127.0.0.1:0", "listen address")
	if err := flags.Parse(args); err != nil {
		return err
	}
	server := rpc.NewServer(log.Global)
	defer server.Stop()
	if err := server.RegisterName("bench", &BenchService{}); err != nil {
		return err
	}
	if err := server.RegisterName("quai", &QuaiService{}); err != nil {
		return err
	}
	listener, err := net.Listen("tcp", *listen)
	if err != nil {
		return err
	}
	mux := http.NewServeMux()
	mux.Handle("/", server)
	mux.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		io.WriteString(w, `{"service":"go-quai-reference","mode":"fixture","chain_verified":false}`)
	})
	json.NewEncoder(os.Stdout).Encode(map[string]interface{}{"ready": true, "address": listener.Addr().String(), "mode": "fixture"})
	httpServer := &http.Server{Handler: mux, ReadHeaderTimeout: 5 * time.Second}
	return httpServer.Serve(listener)
}

func doRequest(ctx context.Context, client *rpc.Client, method string, index, batch, delay int) error {
	values := make([]uint64, batch)
	texts := make([]string, batch)
	payloads := make([]Payload, batch)
	calls := make([]rpc.BatchElem, batch)
	for j := 0; j < batch; j++ {
		sequence := uint64(index*batch + j)
		call := rpc.BatchElem{Method: method}
		switch method {
		case "bench_blockNumber":
			call.Result = &texts[j]
		case "bench_payload":
			call.Args = []interface{}{sequence}
			call.Result = &payloads[j]
		case "bench_echo":
			call.Args = []interface{}{sequence}
			call.Result = &values[j]
		case "bench_delay":
			call.Args = []interface{}{uint64(delay)}
			call.Result = &values[j]
		default:
			return fmt.Errorf("unsupported benchmark method: %s", method)
		}
		calls[j] = call
	}
	if batch == 1 {
		if err := client.CallContext(ctx, calls[0].Result, calls[0].Method, calls[0].Args...); err != nil {
			return err
		}
	} else if err := client.BatchCallContext(ctx, calls); err != nil {
		return err
	}
	for j, call := range calls {
		if call.Error != nil {
			return call.Error
		}
		sequence := uint64(index*batch + j)
		switch method {
		case "bench_blockNumber":
			if texts[j] != "0xa7d8c0" {
				return fmt.Errorf("block number mismatch")
			}
		case "bench_echo":
			if values[j] != sequence {
				return fmt.Errorf("echo mismatch")
			}
		case "bench_delay":
			if values[j] != uint64(delay) {
				return fmt.Errorf("delay mismatch")
			}
		case "bench_payload":
			if payloads[j].Sequence != sequence || payloads[j].Hash != payloadHash || payloads[j].Data != payloadData {
				return fmt.Errorf("payload mismatch")
			}
		}
	}
	return nil
}

func bench(args []string) error {
	flags := flag.NewFlagSet("bench", flag.ContinueOnError)
	url := flags.String("url", "", "server URL")
	requests := flags.Int("requests", 2048, "total HTTP requests")
	concurrency := flags.Int("concurrency", 8, "workers/connections")
	batch := flags.Int("batch", 1, "logical calls per HTTP request")
	method := flags.String("method", "bench_payload", "fixture method")
	delay := flags.Int("delay-ms", 2, "synthetic delay")
	if err := flags.Parse(args); err != nil {
		return err
	}
	if *url == "" || *requests < 1 || *concurrency < 1 || *concurrency > 128 || *batch < 1 || *batch > 128 || *delay < 0 || *delay > 1000 {
		return fmt.Errorf("invalid benchmark arguments")
	}
	transport := &http.Transport{MaxIdleConns: *concurrency, MaxIdleConnsPerHost: *concurrency, MaxConnsPerHost: *concurrency}
	defer transport.CloseIdleConnections()
	client, err := rpc.DialHTTPWithClient(*url, &http.Client{Transport: transport, Timeout: 30 * time.Second})
	if err != nil {
		return err
	}
	defer client.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	var warm sync.WaitGroup
	warmErrors := make(chan error, *concurrency)
	for w := 0; w < *concurrency; w++ {
		warm.Add(1)
		go func(w int) {
			defer warm.Done()
			for i := 0; i < 8; i++ {
				if err := doRequest(ctx, client, *method, w*8+i, *batch, *delay); err != nil {
					warmErrors <- err
					return
				}
			}
		}(w)
	}
	warm.Wait()
	close(warmErrors)
	for err := range warmErrors {
		return fmt.Errorf("warmup: %w", err)
	}
	latencies := make([]int64, *requests)
	var next atomic.Int64
	var failures atomic.Int64
	var firstError string
	var errMu sync.Mutex
	var workers sync.WaitGroup
	start := make(chan struct{})
	for w := 0; w < *concurrency; w++ {
		workers.Add(1)
		go func() {
			defer workers.Done()
			<-start
			for {
				i := int(next.Add(1) - 1)
				if i >= *requests {
					return
				}
				t := time.Now()
				err := doRequest(ctx, client, *method, i, *batch, *delay)
				latencies[i] = time.Since(t).Nanoseconds()
				if err != nil {
					failures.Add(1)
					errMu.Lock()
					if firstError == "" {
						firstError = err.Error()
					}
					errMu.Unlock()
				}
			}
		}()
	}
	t := time.Now()
	close(start)
	workers.Wait()
	elapsed := time.Since(t)
	sort.Slice(latencies, func(i, j int) bool { return latencies[i] < latencies[j] })
	pct := func(p float64) float64 { return float64(latencies[int(math.Ceil(p*float64(len(latencies))))-1]) / 1e6 }
	report := map[string]interface{}{
		"method": *method, "requests": *requests, "logical_calls": *requests * *batch, "batch": *batch, "concurrency": *concurrency,
		"elapsed_seconds": elapsed.Seconds(), "calls_per_second": float64((*requests-int(failures.Load()))**batch) / elapsed.Seconds(),
		"request_p50_ms": pct(.50), "request_p95_ms": pct(.95), "request_p99_ms": pct(.99),
		"failed_requests": failures.Load(), "first_error": firstError,
		"latency_scope": "HTTP request completion; a batch contains multiple calls; warmup excluded",
	}
	json.NewEncoder(os.Stdout).Encode(report)
	if failures.Load() > 0 {
		return fmt.Errorf("%d requests failed", failures.Load())
	}
	return nil
}

func main() {
	log.Global.SetOutput(io.Discard)
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: quai-reference serve|bench [flags]")
		os.Exit(2)
	}
	var err error
	switch os.Args[1] {
	case "serve":
		err = serve(os.Args[2:])
	case "bench":
		err = bench(os.Args[2:])
	default:
		err = fmt.Errorf("unknown command")
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
