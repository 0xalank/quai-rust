//! Parallel proof-of-work pass (ROADMAP Phase 1 step 7). Workers recompute the
//! block proof and the inclusion work of every workshare with their own
//! verifiers. The ordered checker imports the successful results through the
//! verifier memo handoff, so it never trusts a value it could not recompute.
use quai_consensus::{
    pow::{Algorithm, SharedCaches},
    share_rules,
    verifier::{HeaderProofMemo, InclusionWorkMemo, Verifier},
    Error,
};
use quai_primitives::{Hash, Header};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub struct Job {
    pub seq: u64,
    pub header: Header,
    pub uncles: Vec<Header>,
}
pub struct Done {
    pub seq: u64,
    /// The block's PoW hash, or the verifier's refusal.
    pub block: Result<Hash, Error>,
    pub proofs: HeaderProofMemo,
    pub work: InclusionWorkMemo,
    pub shares: usize,
}
pub struct Pool {
    jobs: Option<Sender<Job>>,
    done: Receiver<Done>,
    ready: BTreeMap<u64, Done>,
    workers: Vec<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}
/// Compute the ProgPoW kernels of a job's block header and workshares
/// together (`LightCache::prefetch`), so that the dataset items of their
/// rounds are computed side by side. The checks below then take these
/// results instead of computing each kernel alone; nothing is skipped.
fn prefetch(caches: &SharedCaches, job: &Job) {
    let mut epochs: BTreeMap<u64, Vec<(Hash, u64, u64)>> = BTreeMap::new();
    for h in std::iter::once(&job.header).chain(&job.uncles) {
        let w = h.wire();
        if w.aux_pow.is_some() {
            continue;
        }
        let Some(nonce) = w.nonce else { continue };
        let Ok(height) =
            share_rules::number(w.prime_terminus_number.as_deref().unwrap_or_default())
        else {
            continue;
        };
        if height > Algorithm::Progpow.max_height() {
            continue;
        }
        epochs
            .entry(height / Algorithm::Progpow.epoch_length())
            .or_default()
            .push((h.seal_hash(), nonce, height));
    }
    for inputs in epochs.values() {
        if let Ok(cache) = caches.get(Algorithm::Progpow, inputs[0].2) {
            cache.prefetch(inputs);
        }
    }
}
fn work(verifier: &mut Verifier, job: Job, location: &[u8]) -> Done {
    let block = verifier.verify_header_proof(&job.header).map(|p| p.hash());
    for uncle in &job.uncles {
        // Failures are left to the ordered inclusion checks, which recompute
        // and report them; only successful work is handed over.
        if let Ok(hash) = verifier.precompute_inclusion_work(uncle, location) {
            let w = uncle.wire();
            // Pre-KAWPOW entropy also needs the strong proof of full-target shares.
            if w.aux_pow.is_none()
                && w.difficulty.as_deref().is_some_and(|d| {
                    quai_primitives::meets_blake3_target(&hash, d).unwrap_or(false)
                })
            {
                let _ = verifier.verify_header_proof(uncle);
            }
        }
    }
    Done {
        seq: job.seq,
        block,
        proofs: verifier.take_header_proof_memo(),
        work: verifier.take_inclusion_work_memo(),
        shares: job.uncles.len(),
    }
}
impl Pool {
    pub fn new(threads: usize, caches: Arc<SharedCaches>, location: Vec<u8>) -> Self {
        let (job_tx, job_rx) = channel::<Job>();
        let (done_tx, done_rx) = channel::<Done>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let stop = Arc::new(AtomicBool::new(false));
        let workers = (0..threads.max(1))
            .filter_map(|i| {
                let jobs = job_rx.clone();
                let done = done_tx.clone();
                let caches = caches.clone();
                let location = location.clone();
                let stop = stop.clone();
                std::thread::Builder::new()
                    .name(format!("pow-{i}"))
                    .spawn(move || {
                        let mut verifier = Verifier::with_shared_caches(caches.clone());
                        loop {
                            let job = match jobs.lock() {
                                Ok(rx) => rx.recv(),
                                Err(_) => return,
                            };
                            let Ok(job) = job else { return };
                            if stop.load(Ordering::Relaxed) {
                                return;
                            }
                            prefetch(&caches, &job);
                            if done.send(work(&mut verifier, job, &location)).is_err() {
                                return;
                            }
                        }
                    })
                    .ok()
            })
            .collect();
        Self {
            jobs: Some(job_tx),
            done: done_rx,
            ready: BTreeMap::new(),
            workers,
            stop,
        }
    }
    pub fn submit(&self, job: Job) {
        // A send error means every worker has stopped; `wait` reports it.
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(job);
        }
    }
    /// Block until the result for `seq` is available.
    pub fn wait(&mut self, seq: u64) -> Result<Done, Error> {
        loop {
            if let Some(d) = self.ready.remove(&seq) {
                return Ok(d);
            }
            match self.done.recv() {
                Ok(d) => {
                    self.ready.insert(d.seq, d);
                }
                Err(_) => return Err(Error::Unavailable("proof-of-work workers stopped")),
            }
        }
    }
}
impl Drop for Pool {
    fn drop(&mut self) {
        // Queued jobs are abandoned; a worker finishes at most its current one.
        self.stop.store(true, Ordering::Relaxed);
        self.jobs.take();
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}
