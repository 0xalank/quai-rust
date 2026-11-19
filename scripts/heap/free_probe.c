// free_probe.c: a DYLD interposer for free() on macOS, used by
// scripts/stress_tests.sh --probe. It learns libmalloc's xzone freelist
// cookie at load time and counts every block passed to free() whose first
// word carries the free cookie (zone cookie ^ address) in its upper 7 bytes
// ("near") or in all 8 ("exact"). libmalloc 792.x (macOS 26.1-26.3) aborts
// on an exact hit at block index 2 or later of its chunk with "pointer being
// freed was not allocated", although the block is live. Near hits show code
// that copies stale bytes, which can hold that cookie, into the first word of
// a heap block. The first 40 hits print their address, first two words and
// return addresses; "main" gives the slide for atos.
//
//   clang -O2 -dynamiclib -o free_probe.dylib scripts/heap/free_probe.c
//   DYLD_INSERT_LIBRARIES=$PWD/free_probe.dylib <binary>
//
// The fast path has no stack frame and tail-calls free(), so libmalloc runs
// at its usual stack depth. Diagnostic only; not part of any build.
#include <stdint.h>
#include <stdlib.h>
#include <unistd.h>
#include <stdatomic.h>
#include <execinfo.h>
#include <mach-o/dyld.h>
#include <dlfcn.h>

static uint64_t cookie;
static _Atomic uint64_t n_near, n_exact;
static _Atomic int n_reports;

static void put(const char *s, size_t n) { (void)!write(2, s, n); }
static void put_hex(uint64_t v) {
    char b[19]; b[0] = '0'; b[1] = 'x';
    for (int i = 15; i >= 0; i--) { int d = v & 15; b[2 + i] = d < 10 ? '0' + d : 'a' + d - 10; v >>= 4; }
    b[18] = ' '; put(b, 19);
}

__attribute__((constructor)) static void learn(void) {
    enum { N = 512 };
    static void *blocks[N];
    static uint64_t vals[N];
    for (int i = 0; i < N; i++) blocks[i] = malloc(48);
    for (int i = 0; i < N; i++) free(blocks[i]);
    for (int i = 0; i < N; i++) vals[i] = *(volatile uint64_t *)blocks[i] ^ (uint64_t)blocks[i];
    // majority vote
    uint64_t best = 0; int best_n = 0;
    for (int i = 0; i < N; i += 7) {
        int c = 0;
        for (int j = 0; j < N; j++) c += vals[j] == vals[i];
        if (c > best_n) { best_n = c; best = vals[i]; }
    }
    if (best_n > N / 2) cookie = best;
    put("FREEPROBE cookie ", 17); put_hex(cookie); put("votes ", 6); put_hex(best_n);
    put("main ", 5); put_hex((uint64_t)dlsym(RTLD_MAIN_ONLY, "main")); put("\n", 1);
}

__attribute__((destructor)) static void summary(void) {
    put("FREEPROBE summary near ", 23); put_hex(n_near); put("exact ", 6); put_hex(n_exact); put("\n", 1);
}

__attribute__((noinline, cold)) static void report(void *p, uint64_t w, int exact) {
    if (exact) n_exact++; else n_near++;
    if (atomic_fetch_add(&n_reports, 1) >= 40) return;
    put(exact ? "FREEPROBE exact ptr " : "FREEPROBE near ptr ", exact ? 20 : 19);
    put_hex((uint64_t)p); put("word0 ", 6); put_hex(w); put("word1 ", 6); put_hex(((volatile uint64_t *)p)[1]);
    put("stack ", 6);
    void *frames[10];
    int n = backtrace(frames, 10);
    for (int i = 1; i < n; i++) put_hex((uint64_t)frames[i]);
    put("\n", 1);
}

void probe_free(void *p) {
    if (p && cookie) {
        uint64_t w = *(volatile uint64_t *)p;
        uint64_t f = cookie ^ (uint64_t)p;
        if (__builtin_expect((w ^ f) >> 8 == 0, 0)) report(p, w, w == f);
    }
    free(p);
}

__attribute__((used)) static struct { const void *replacement; const void *replacee; } interpose_free
    __attribute__((section("__DATA,__interpose"))) = { (const void *)&probe_free, (const void *)&free };
