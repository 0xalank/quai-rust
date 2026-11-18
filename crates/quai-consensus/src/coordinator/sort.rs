// Copyright 2022 The Go Authors. All rights reserved.
// Rust adaptation of Go 1.24.5 sort/zsortfunc.go. BSD license in NOTICE.md.
// Retains source ordering of equal entropy candidates; this is not stable sort.
pub(super) fn sort<T, F: Fn(&T, &T) -> bool>(v: &mut [T], less: F) {
    let limit = usize::BITS as usize - v.len().leading_zeros() as usize;
    pdq(v, 0, v.len(), limit, &less)
}
fn insertion<T, F: Fn(&T, &T) -> bool>(v: &mut [T], a: usize, b: usize, f: &F) {
    for i in a + 1..b {
        let mut j = i;
        while j > a && f(&v[j], &v[j - 1]) {
            v.swap(j, j - 1);
            j -= 1
        }
    }
}
fn sift<T, F: Fn(&T, &T) -> bool>(v: &mut [T], mut root: usize, hi: usize, first: usize, f: &F) {
    loop {
        let mut child = 2 * root + 1;
        if child >= hi {
            break;
        }
        if child + 1 < hi && f(&v[first + child], &v[first + child + 1]) {
            child += 1
        }
        if !f(&v[first + root], &v[first + child]) {
            break;
        }
        v.swap(first + root, first + child);
        root = child
    }
}
fn heap<T, F: Fn(&T, &T) -> bool>(v: &mut [T], a: usize, b: usize, f: &F) {
    let n = b - a;
    for i in (0..=((n - 1) / 2)).rev() {
        sift(v, i, n, a, f)
    }
    for i in (0..n).rev() {
        v.swap(a, a + i);
        sift(v, 0, i, a, f)
    }
}
fn order<T, F: Fn(&T, &T) -> bool>(
    v: &[T],
    a: usize,
    b: usize,
    s: &mut usize,
    f: &F,
) -> (usize, usize) {
    if f(&v[b], &v[a]) {
        *s += 1;
        (b, a)
    } else {
        (a, b)
    }
}
fn median<T, F: Fn(&T, &T) -> bool>(
    v: &[T],
    a: usize,
    b: usize,
    c: usize,
    s: &mut usize,
    f: &F,
) -> usize {
    let (a, b) = order(v, a, b, s, f);
    let (b, _) = order(v, b, c, s, f);
    let (_, b) = order(v, a, b, s, f);
    b
}
fn pivot<T, F: Fn(&T, &T) -> bool>(v: &[T], a: usize, b: usize, f: &F) -> (usize, u8) {
    let l = b - a;
    let (mut i, mut j, mut k) = (a + l / 4, a + l / 4 * 2, a + l / 4 * 3);
    let mut s = 0;
    if l >= 8 {
        if l >= 50 {
            i = median(v, i - 1, i, i + 1, &mut s, f);
            j = median(v, j - 1, j, j + 1, &mut s, f);
            k = median(v, k - 1, k, k + 1, &mut s, f)
        }
        j = median(v, i, j, k, &mut s, f)
    }
    (
        j,
        if s == 0 {
            1
        } else if s == 12 {
            2
        } else {
            0
        },
    )
}
fn partial<T, F: Fn(&T, &T) -> bool>(v: &mut [T], a: usize, b: usize, f: &F) -> bool {
    let mut i = a + 1;
    for _ in 0..5 {
        while i < b && !f(&v[i], &v[i - 1]) {
            i += 1
        }
        if i == b {
            return true;
        }
        if b - a < 50 {
            return false;
        }
        v.swap(i, i - 1);
        if i - a >= 2 {
            let mut j = i - 1;
            while j >= 1 {
                if !f(&v[j], &v[j - 1]) {
                    break;
                }
                v.swap(j, j - 1);
                j -= 1
            }
        }
        if b - i >= 2 {
            for j in i + 1..b {
                if !f(&v[j], &v[j - 1]) {
                    break;
                }
                v.swap(j, j - 1)
            }
        }
    }
    false
}
fn partition<T, F: Fn(&T, &T) -> bool>(
    v: &mut [T],
    a: usize,
    b: usize,
    p: usize,
    equal: bool,
    f: &F,
) -> (usize, bool) {
    v.swap(a, p);
    let (mut i, mut j) = (a + 1, b - 1);
    let left = |v: &[T], i| {
        if equal {
            !f(&v[a], &v[i])
        } else {
            f(&v[i], &v[a])
        }
    };
    let right = |v: &[T], j| {
        if equal {
            f(&v[a], &v[j])
        } else {
            !f(&v[j], &v[a])
        }
    };
    while i <= j && left(v, i) {
        i += 1
    }
    while i <= j && right(v, j) {
        j -= 1
    }
    if i > j {
        if !equal {
            v.swap(j, a)
        }
        return (if equal { i } else { j }, true);
    }
    loop {
        v.swap(i, j);
        i += 1;
        j -= 1;
        while i <= j && left(v, i) {
            i += 1
        }
        while i <= j && right(v, j) {
            j -= 1
        }
        if i > j {
            break;
        }
    }
    if !equal {
        v.swap(j, a)
    }
    (if equal { i } else { j }, false)
}
fn patterns<T>(v: &mut [T], a: usize, b: usize) {
    let l = b - a;
    if l >= 8 {
        let mut r = l as u64;
        let modulus = 1usize << (usize::BITS - l.leading_zeros());
        for idx in a + l / 4 * 2 - 1..=a + l / 4 * 2 + 1 {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            let mut other = r as usize & (modulus - 1);
            if other >= l {
                other -= l
            }
            v.swap(idx, a + other)
        }
    }
}
fn pdq<T, F: Fn(&T, &T) -> bool>(v: &mut [T], mut a: usize, mut b: usize, mut limit: usize, f: &F) {
    let (mut balanced, mut partitioned) = (true, true);
    loop {
        let n = b - a;
        if n <= 12 {
            insertion(v, a, b, f);
            return;
        }
        if limit == 0 {
            heap(v, a, b, f);
            return;
        }
        if !balanced {
            patterns(v, a, b);
            limit -= 1
        }
        let (mut p, mut hint) = pivot(v, a, b, f);
        if hint == 2 {
            v[a..b].reverse();
            p = b - 1 - (p - a);
            hint = 1
        }
        if balanced && partitioned && hint == 1 && partial(v, a, b, f) {
            return;
        }
        if a > 0 && !f(&v[a - 1], &v[p]) {
            a = partition(v, a, b, p, true, f).0;
            continue;
        }
        let (mid, already) = partition(v, a, b, p, false, f);
        partitioned = already;
        let (left, right) = (mid - a, b - mid);
        if left < right {
            balanced = left >= n / 8;
            pdq(v, a, mid, limit, f);
            a = mid + 1
        } else {
            balanced = right >= n / 8;
            pdq(v, mid + 1, b, limit, f);
            b = mid
        }
    }
}
