// Read-only load generator. Every response is checked against one frozen result.
package main

import (
	"bytes"
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"math"
	"net"
	"net/http"
	urlpkg "net/url"
	"os"
	"sort"
	"sync"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/rpc"
)

func canonical(raw []byte) ([]byte, error) {
	var value interface{}
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	if err := decoder.Decode(&value); err != nil {
		return nil, err
	}
	return json.Marshal(value)
}

func main() {
	log.Global.SetOutput(io.Discard)
	url := flag.String("url", "", "loopback server")
	method := flag.String("method", "quai_getBlockByHash", "read method")
	paramsPath := flag.String("params", "", "JSON positional params file")
	expectedPath := flag.String("expected", "", "JSON result file")
	seconds := flag.Int("seconds", 2, "duration, seconds")
	concurrency := flag.Int("concurrency", 8, "HTTP concurrency")
	flag.Parse()
	parsed, e := urlpkg.Parse(*url)
	if e != nil || parsed.Scheme != "http" || parsed.User != nil || (parsed.Hostname() != "localhost" && (net.ParseIP(parsed.Hostname()) == nil || !net.ParseIP(parsed.Hostname()).IsLoopback())) {
		panic("load generator requires loopback HTTP")
	}
	if *seconds < 1 || *seconds > 30 || *concurrency < 1 || *concurrency > 128 {
		panic("unsupported load limits")
	}
	if *method != "quai_getBlockByHash" && *method != "quai_getTransactionReceipt" {
		panic("only immutable block/receipt read workloads are supported")
	}
	input, err := os.ReadFile(*paramsPath)
	if err != nil {
		panic(err)
	}
	var params []interface{}
	if err = json.Unmarshal(input, &params); err != nil {
		panic(err)
	}
	expected, err := os.ReadFile(*expectedPath)
	if err != nil {
		panic(err)
	}
	expected, err = canonical(expected)
	if err != nil {
		panic(err)
	}
	transport := &http.Transport{MaxIdleConns: *concurrency, MaxIdleConnsPerHost: *concurrency, MaxConnsPerHost: *concurrency}
	defer transport.CloseIdleConnections()
	client, err := rpc.DialHTTPWithClient(*url, &http.Client{Transport: transport, Timeout: 5 * time.Second})
	if err != nil {
		panic(err)
	}
	defer client.Close()
	invoke := func() error {
		var result json.RawMessage
		if err := client.CallContext(context.Background(), &result, *method, params...); err != nil {
			return err
		}
		actual, err := canonical(result)
		if err != nil {
			return err
		}
		if !bytes.Equal(actual, expected) {
			return fmt.Errorf("result mismatch")
		}
		return nil
	}
	for i := 0; i < *concurrency*4; i++ {
		if err := invoke(); err != nil {
			panic(err)
		}
	}
	var total, failed atomic.Uint64
	var mu sync.Mutex
	var latencies []int64
	var firstError string
	var workers sync.WaitGroup
	var usageStart syscall.Rusage
	syscall.Getrusage(syscall.RUSAGE_SELF, &usageStart)
	start := time.Now()
	deadline := start.Add(time.Duration(*seconds) * time.Second)
	for worker := 0; worker < *concurrency; worker++ {
		workers.Add(1)
		go func() {
			defer workers.Done()
			local := make([]int64, 0, 10000)
			for time.Now().Before(deadline) {
				t := time.Now()
				err := invoke()
				elapsed := time.Since(t).Nanoseconds()
				total.Add(1)
				if err != nil {
					failed.Add(1)
					mu.Lock()
					if firstError == "" {
						firstError = err.Error()
					}
					mu.Unlock()
				}
				local = append(local, elapsed)
			}
			mu.Lock()
			latencies = append(latencies, local...)
			mu.Unlock()
		}()
	}
	workers.Wait()
	elapsed := time.Since(start).Seconds()
	sort.Slice(latencies, func(i, j int) bool { return latencies[i] < latencies[j] })
	pct := func(p float64) float64 {
		if len(latencies) == 0 {
			return 0
		}
		return float64(latencies[int(math.Ceil(p*float64(len(latencies))))-1]) / 1e6
	}
	var usageEnd syscall.Rusage
	syscall.Getrusage(syscall.RUSAGE_SELF, &usageEnd)
	cpuSeconds := float64(usageEnd.Utime.Sec-usageStart.Utime.Sec+usageEnd.Stime.Sec-usageStart.Stime.Sec) + float64(usageEnd.Utime.Usec-usageStart.Utime.Usec+usageEnd.Stime.Usec-usageStart.Stime.Usec)/1e6
	result := map[string]interface{}{"load_generator_cpu_seconds": cpuSeconds, "method": *method, "concurrency": *concurrency, "calls": total.Load(), "failed_requests": failed.Load(), "first_error": firstError, "elapsed_seconds": elapsed, "calls_per_second": float64(total.Load()-failed.Load()) / elapsed, "p50_ms": pct(.5), "p95_ms": pct(.95), "p99_ms": pct(.99), "validated_every_response": true}
	json.NewEncoder(os.Stdout).Encode(result)
	if failed.Load() != 0 {
		os.Exit(1)
	}
}
