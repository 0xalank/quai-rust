//! Optional private-profile node runner. Native admission owns all chain state;
//! stdin is a bounded local control channel, never a public RPC endpoint.
use libp2p::{identity, PeerId};
use quai_consensus::private_canonical::{BlockBundle, PrivateCanonical};
use quai_network::{
    private_sync,
    transport::{Event, PrivatePeer, WireStream},
};
use quai_primitives::{decode_hex, encode_hex, Hash};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{mpsc, Notify},
    task::{JoinHandle, JoinSet},
};
const PROFILE: &str = "private-hierarchy-v1";
const MAX_COMMAND: usize = 64 << 10;
const MAX_PACK: usize = 64 << 20;
fn bad(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}
fn need(ok: bool, message: &str) -> io::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(bad(message))
    }
}
fn text<'a>(v: &'a Value, key: &str) -> io::Result<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| bad(format!("missing string {key}")))
}
fn hex(v: &Value, key: &str) -> io::Result<Vec<u8>> {
    decode_hex(text(v, key)?).map_err(bad)
}
fn hash(v: &Value, key: &str) -> io::Result<Hash> {
    hex(v, key)?.try_into().map_err(|_| bad("hash width"))
}
fn output(v: &Value) -> io::Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, v).map_err(bad)?;
    out.write_all(b"\n")?;
    out.flush()
}
fn reply(id: Value, result: io::Result<Value>) -> io::Result<()> {
    match result {
        Ok(v) => output(&json!({"id":id,"ok":true,"result":v})),
        Err(e) => output(
            &json!({"id":id,"ok":false,"error":{"kind":format!("{:?}",e.kind()),"message":e.to_string()}}),
        ),
    }
}
fn lock(
    chain: &Mutex<PrivateCanonical>,
) -> io::Result<std::sync::MutexGuard<'_, PrivateCanonical>> {
    chain.lock().map_err(|_| bad("chain worker panicked"))
}
fn read_bounded(path: &Path, max: usize) -> io::Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    need(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= max as u64,
        "file type/byte limit",
    )?;
    let mut raw = vec![];
    File::open(path)?
        .take(max as u64 + 1)
        .read_to_end(&mut raw)?;
    need(raw.len() <= max, "file grew beyond byte limit")?;
    Ok(raw)
}
fn identity_key(root: &Path) -> io::Result<identity::Keypair> {
    let key = root.join("peer.key");
    let pending = root.join("peer.key.pending");
    let present = |p: &Path| -> io::Result<bool> {
        match fs::symlink_metadata(p) {
            Ok(m) => {
                need(
                    m.is_file() && !m.file_type().is_symlink(),
                    "peer identity file type",
                )?;
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    };
    let key_exists = present(&key)?;
    let pending_exists = present(&pending)?;
    need(
        !(key_exists && pending_exists),
        "ambiguous peer identity files",
    )?;
    let read_key = |p: &Path| -> io::Result<identity::Keypair> {
        let raw = read_bounded(p, 256)?;
        const MAGIC: &[u8] = b"QUAI-PRIVATE-PEER-1\n";
        need(
            raw.starts_with(MAGIC) && raw.len() > MAGIC.len() + 32,
            "peer identity encoding",
        )?;
        let end = raw.len() - 32;
        need(
            quai_primitives::transaction::keccak(&raw[..end]).as_slice() == &raw[end..],
            "peer identity checksum",
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            need(
                fs::metadata(p)?.permissions().mode() & 0o077 == 0,
                "peer identity requires owner-only permissions",
            )?;
        }
        identity::Keypair::from_protobuf_encoding(&raw[MAGIC.len()..end]).map_err(bad)
    };
    if key_exists {
        return read_key(&key);
    }
    let result = if pending_exists {
        read_key(&pending)?
    } else {
        let k = identity::Keypair::generate_ed25519();
        let mut raw = b"QUAI-PRIVATE-PEER-1\n".to_vec();
        raw.extend(k.to_protobuf_encoding().map_err(bad)?);
        let checksum = quai_primitives::transaction::keccak(&raw);
        raw.extend(checksum);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut f = options.open(&pending)?;
        f.write_all(&raw)?;
        f.sync_all()?;
        k
    };
    fs::rename(&pending, &key)?;
    File::open(root)?.sync_all()?;
    Ok(result)
}
fn open(root: &Path, now: u64, profile: &str) -> io::Result<(PrivateCanonical, identity::Keypair)> {
    if !root.exists() {
        fs::create_dir_all(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        }
    }
    let meta = fs::symlink_metadata(root)?;
    need(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "node datadir type",
    )?;
    for e in fs::read_dir(root)? {
        let e = e?;
        need(
            matches!(
                e.file_name().to_str(),
                Some("chain" | "peer.key" | "peer.key.pending")
            ),
            "unknown private-node datadir entry",
        )?;
    }
    // Acquire the existing canonical writer lock before creating/recovering identity.
    let chain = match profile {
        "private-execution-v3" => PrivateCanonical::open_execution_v3(&root.join("chain"), now)?,
        "private-execution-v2" => PrivateCanonical::open_execution_v2(&root.join("chain"), now)?,
        PROFILE => PrivateCanonical::open(&root.join("chain"), now)?,
        _ => return Err(bad("unreviewed runtime contract")),
    };
    let key = identity_key(root)?;
    File::open(root)?.sync_all()?;
    Ok((chain, key))
}
fn bundle(v: &Value) -> io::Result<BlockBundle> {
    let mut contexts = vec![];
    for c in v
        .get("contexts")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("missing contexts"))?
    {
        let context = c
            .get("context")
            .and_then(Value::as_u64)
            .ok_or_else(|| bad("context number"))?;
        need(context < 2, "inactive context")?;
        contexts.push((context as u8, hex(c, "bytes")?));
    }
    let b = BlockBundle {
        zone: hex(v, "zone")?,
        contexts,
    };
    b.checked()?;
    Ok(b)
}
#[derive(Default)]
struct Served {
    requests: AtomicU64,
    bytes: AtomicU64,
}
fn status(chain: &PrivateCanonical, served: &Served) -> io::Result<Value> {
    let v = chain.view()?;
    Ok(
        json!({"profile":chain.profile_name(),"activated_execution":chain.activated_execution(),"all_defined_etx_execution":chain.all_etx_execution(),"private_profile_canonical":true,"global_full_consensus":false,"canonical_public_sync":false,"head":encode_hex(&v.block()),"height":v.height(),"checkpoint":encode_hex(&v.checkpoint()),"evm_root":encode_hex(&v.roots().evm),"etx_root":encode_hex(&v.roots().etx),"utxo_root":encode_hex(&v.roots().utxo),"requests_served":served.requests.load(Ordering::Relaxed),"response_payload_bytes_served":served.bytes.load(Ordering::Relaxed),"storage_backend":chain.storage_backend(),"indexed_ancestry":chain.indexed_ancestry(),"indexed_catalog":chain.indexed_catalog(),"lazy_qi":chain.lazy_qi(),"lazy_accounts":chain.lazy_accounts(),"lazy_storage":chain.lazy_storage(),"lazy_etx":chain.lazy_etx(),"lazy_lockups":chain.lazy_lockups(),"admitted_blocks":chain.admitted_count(),"canonical_execution_uses_state_database":chain.storage_backend()=="incremental-trie-database","durable_checkpoint":encode_hex(&chain.durable_checkpoint()?),"runtime_go_dependency":false,"full_proof_inclusion_reuses":chain.full_proof_inclusion_reuses(),"history_decode_stats":chain.history_decode_stats(),"process_id":std::process::id()}),
    )
}
fn applied(v: private_sync::Applied) -> Value {
    json!({"fetched_blocks":v.fetched_blocks,"imported_blocks":v.imported_blocks,"selected_changes":v.selected_changes,"reorgs":v.reorgs,"requests":v.requests,"reused_header_proofs":v.reused_header_proofs,"header_proof_hits":v.header_proof_hits,"header_proof_misses":v.header_proof_misses,"response_payload_bytes":v.bytes,"head":encode_hex(&v.head),"height":v.height,"global_full_consensus":false,"canonical_public_sync":false})
}
async fn serve_stream(
    mut stream: WireStream,
    chain: Arc<Mutex<PrivateCanonical>>,
    stats: Arc<Served>,
) {
    loop {
        let raw = match stream.read().await {
            Ok(raw) => raw,
            Err(_) => break,
        };
        let c = chain.clone();
        let result =
            tokio::task::spawn_blocking(move || private_sync::serve(&*lock(&c)?, &raw)).await;
        let reply = match result {
            Ok(Ok(v)) => v,
            _ => break,
        };
        if stream.write(&reply).await.is_err() {
            break;
        }
        stats.requests.fetch_add(1, Ordering::Relaxed);
        stats.bytes.fetch_add(reply.len() as u64, Ordering::Relaxed);
    }
    let _ = stream.close().await;
}
struct Job {
    id: Value,
    cancel: Arc<AtomicBool>,
    notify: Arc<Notify>,
    task: JoinHandle<io::Result<Value>>,
}
async fn follow_job(
    peer: Arc<PrivatePeer>,
    chain: Arc<Mutex<PrivateCanonical>>,
    remote: PeerId,
    address: libp2p::Multiaddr,
    head: [u8; 32],
    height: u64,
    limit: usize,
    now: u64,
    cancelled: Arc<AtomicBool>,
    wake: Arc<Notify>,
) -> io::Result<Value> {
    use private_sync::forward;
    need(
        quai_network::transport::loopback_address(&address),
        "private node requires loopback peer address",
    )?;
    let (local, known) = tokio::task::spawn_blocking({
        let c = chain.clone();
        move || -> io::Result<_> {
            let c = lock(&c)?;
            let known = c.contains(&head)?;
            if known {
                need(
                    c.bundle(head)?.checked()?.number(2).map_err(bad)? == height,
                    "known forward target height",
                )?;
            }
            Ok((c.view()?, known))
        }
    })
    .await
    .map_err(bad)??;
    if known {
        return tokio::task::spawn_blocking(move || {
            private_sync::Acquired::new(vec![], 0, 0)
                .apply_cancellable(&mut *lock(&chain)?, now, &cancelled)
                .map(|v| {
                    let mut q = applied(v);
                    q["windows"] = json!(0);
                    q["target_admitted"] = json!(true);
                    q["target_head"] = json!(encode_hex(&head));
                    q
                })
        })
        .await
        .map_err(bad)?;
    }
    need(
        !cancelled.load(Ordering::Acquire),
        "forward acquisition cancelled",
    )?;
    peer.enroll(remote)?;
    peer.dial(remote, address).await?;
    let begin = forward::begin(&peer, remote, &local, head, height);
    let mut plan = tokio::select! {r=begin=>r?,_=wake.notified()=>return Err(io::Error::new(io::ErrorKind::Interrupted,"forward acquisition cancelled"))};
    // Discovery is complete. Release its pinned catalog generation before import.
    drop(local);
    let mut requests = plan.discovery_requests;
    let mut bytes = plan.discovery_bytes;
    let mut fetched = 0usize;
    let mut imported = 0usize;
    let mut selected = 0usize;
    let mut reorgs = 0usize;
    let mut windows = 0usize;
    let mut peak_window_response_bytes = 0usize;
    let mut peak_window_blocks = 0usize;
    let mut verifier = quai_consensus::verifier::Verifier::default();
    let mut reused_header_proofs = 0usize;
    let mut header_proof_hits = 0u64;
    let mut header_proof_misses = 0u64;
    while !plan.done() {
        if cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "forward acquisition cancelled",
            ));
        }
        let get = forward::window_with_verifier(&peer, remote, &plan, limit, &mut verifier);
        let window = tokio::select! {r=get=>r?,_=wake.notified()=>return Err(io::Error::new(io::ErrorKind::Interrupted,"forward acquisition cancelled"))};
        let next = window.next;
        let acquired = window.acquired;
        peak_window_response_bytes = peak_window_response_bytes.max(acquired.bytes);
        peak_window_blocks = peak_window_blocks.max(acquired.blocks.len());
        let c = chain.clone();
        let stop = cancelled.clone();
        let result = tokio::task::spawn_blocking(move || {
            acquired.apply_cancellable(&mut *lock(&c)?, now, &stop)
        })
        .await
        .map_err(bad)??;
        requests = requests
            .checked_add(result.requests)
            .ok_or_else(|| bad("forward request count overflow"))?;
        bytes = bytes
            .checked_add(result.bytes)
            .ok_or_else(|| bad("forward byte count overflow"))?;
        fetched = fetched
            .checked_add(result.fetched_blocks)
            .ok_or_else(|| bad("forward block count overflow"))?;
        imported = imported
            .checked_add(result.imported_blocks)
            .ok_or_else(|| bad("forward import count overflow"))?;
        selected = selected
            .checked_add(result.selected_changes)
            .ok_or_else(|| bad("forward selection count overflow"))?;
        reorgs = reorgs
            .checked_add(result.reorgs)
            .ok_or_else(|| bad("forward reorg count overflow"))?;
        reused_header_proofs += result.reused_header_proofs;
        header_proof_hits += result.header_proof_hits;
        header_proof_misses += result.header_proof_misses;
        windows += 1;
        plan = next;
    }
    tokio::task::spawn_blocking(move||{let c=lock(&chain)?;need(c.contains(&head)?,"forward target lacks native admission")?;let view=c.view()?;Ok(json!({"fetched_blocks":fetched,"imported_blocks":imported,"selected_changes":selected,"reorgs":reorgs,"requests":requests,"response_payload_bytes":bytes,"windows":windows,"window_block_limit":limit,"peak_window_response_bytes":peak_window_response_bytes,"peak_window_blocks":peak_window_blocks,"reused_header_proofs":reused_header_proofs,"header_proof_hits":header_proof_hits,"header_proof_misses":header_proof_misses,"target_admitted":true,"target_head":encode_hex(&head),"height":view.height(),"head":encode_hex(&view.block()),"global_full_consensus":false,"canonical_public_sync":false}))}).await.map_err(bad)?
}
fn start_job(
    command: Value,
    peer: Arc<PrivatePeer>,
    chain: Arc<Mutex<PrivateCanonical>>,
    now: u64,
) -> io::Result<Job> {
    let id = command.get("id").cloned().unwrap_or(Value::Null);
    let op = text(&command, "op")?.to_owned();
    need(
        matches!(op.as_str(), "sync" | "follow" | "import"),
        "unknown worker operation",
    )?;
    // Validate cheap command fields before claiming the single worker slot.
    let remote = if op == "sync" || op == "follow" {
        Some((
            text(&command, "peer_id")?.parse::<PeerId>().map_err(bad)?,
            text(&command, "address")?
                .parse::<libp2p::Multiaddr>()
                .map_err(bad)?,
            hash(&command, "head")?,
        ))
    } else {
        None
    };
    let path = if op == "import" {
        Some(PathBuf::from(text(&command, "path")?))
    } else {
        None
    };
    let forward_height = if op == "follow" {
        Some(
            command["height"]
                .as_u64()
                .ok_or_else(|| bad("forward target height required"))?,
        )
    } else {
        None
    };
    let forward_limit = command
        .get("window_blocks")
        .map(|n| {
            n.as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| bad("forward window block limit"))
        })
        .transpose()?
        .unwrap_or(private_sync::forward::MAX_WINDOW_BLOCKS);
    need(
        (1..=private_sync::forward::MAX_WINDOW_BLOCKS).contains(&forward_limit),
        "forward window block limit",
    )?;
    let cancel = Arc::new(AtomicBool::new(false));
    let notify = Arc::new(Notify::new());
    let cancelled = cancel.clone();
    let wake = notify.clone();
    let task = tokio::spawn(async move {
        if let Some((remote, address, head)) = remote {
            if let Some(height) = forward_height {
                return follow_job(
                    peer,
                    chain,
                    remote,
                    address,
                    head,
                    height,
                    forward_limit,
                    now,
                    cancelled,
                    wake,
                )
                .await;
            }
            // Public/DNS addresses fail before dial. Enrollment is a local explicit command.
            need(
                quai_network::transport::loopback_address(&address),
                "private node requires loopback peer address",
            )?;
            let known = tokio::task::spawn_blocking({
                let c = chain.clone();
                move || lock(&c)?.admitted_view()
            })
            .await
            .map_err(bad)??;
            if known.contains(&head)? {
                return tokio::task::spawn_blocking(move || {
                    private_sync::Acquired::new(vec![], 0, 0)
                        .apply_cancellable(&mut *lock(&chain)?, now, &cancelled)
                        .map(applied)
                })
                .await
                .map_err(bad)?;
            }
            peer.enroll(remote)?;
            let acquire = async {
                peer.dial(remote, address).await?;
                private_sync::acquire(&peer, remote, head, &known).await
            };
            let got = tokio::select! {r=acquire=>r?,_=wake.notified()=>return Err(io::Error::new(io::ErrorKind::Interrupted,"private sync cancelled"))};
            tokio::task::spawn_blocking(move || {
                got.apply_cancellable(&mut *lock(&chain)?, now, &cancelled)
                    .map(applied)
            })
            .await
            .map_err(bad)?
        } else {
            tokio::task::spawn_blocking(move||{
    let raw=read_bounded(path.as_ref().unwrap(),MAX_PACK)?;let pack: Value=serde_json::from_slice(&raw).map_err(bad)?;
    let rows=pack.as_array().ok_or_else(||bad("bundle pack must be an array"))?;need(rows.len()<4096,"bundle pack record limit")?;
    let mut count=0;let mut changes=0;let mut reorgs=0;
    for row in rows {
     if cancelled.load(Ordering::Acquire){return Err(io::Error::new(io::ErrorKind::Interrupted,"private import cancelled between blocks"));}
     let block=bundle(row)?;let r=lock(&chain)?.import(block,now)?;count+=usize::from(!r.duplicate);changes+=usize::from(r.selected);reorgs+=usize::from(!r.detached.is_empty());
    }
    let view=lock(&chain)?.view()?;Ok(json!({"imported_blocks":count,"selected_changes":changes,"reorgs":reorgs,"head":encode_hex(&view.block()),"height":view.height(),"global_full_consensus":false}))
   }).await.map_err(bad)?
        }
    });
    Ok(Job {
        id,
        cancel,
        notify,
        task,
    })
}
fn control_input(tx: mpsc::Sender<io::Result<Value>>) {
    let mut input = BufReader::new(io::stdin());
    let mut line = vec![];
    loop {
        let part = match input.fill_buf() {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.blocking_send(Err(e));
                return;
            }
        };
        if part.is_empty() {
            if !line.is_empty() {
                let _ = tx.blocking_send(Err(bad("truncated control command")));
            }
            return;
        }
        let end = part.iter().position(|b| *b == b'\n').map(|n| n + 1);
        let take = end.unwrap_or(part.len());
        if line.len() + take > MAX_COMMAND {
            let _ = tx.blocking_send(Err(bad("control command byte limit")));
            return;
        }
        line.extend_from_slice(&part[..take]);
        input.consume(take);
        if end.is_some() {
            let result = serde_json::from_slice(&line).map_err(bad);
            line.clear();
            if tx.blocking_send(result).is_err() {
                return;
            }
        }
    }
}
async fn run() -> io::Result<()> {
    let mut root = None;
    let mut now = None;
    let mut profile = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().ok_or_else(|| bad("expected flag value"))?;
        match arg.as_str() {
            "--datadir" => root = Some(PathBuf::from(value)),
            "--profile" => profile = Some(value),
            "--now" => now = Some(value.parse::<u64>().map_err(bad)?),
            _ => return Err(bad("unknown private-node option")),
        }
    }
    need(
        matches!(
            profile.as_deref(),
            Some(PROFILE | "private-execution-v2" | "private-execution-v3")
        ),
        "explicit reviewed --profile is required",
    )?;
    let root = root.ok_or_else(|| bad("--datadir is required"))?;
    let now = now.unwrap_or(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(bad)?
            .as_secs(),
    );
    let (chain, key) = open(&root, now, profile.as_deref().unwrap())?;
    let runtime_profile = chain.profile_name();
    let admissions = chain.admission_progress();
    let chain = Arc::new(Mutex::new(chain));
    let peer = Arc::new(PrivatePeer::bind_with_identity(key).await?);
    let stats = Arc::new(Served::default());
    output(
        &json!({"event":"ready","peer_id":peer.id.to_string(),"address":peer.address.to_string(),"status":status(&*lock(&chain)?,&stats)?}),
    )?;
    let (tx, mut rx) = mpsc::channel(8);
    // A detached console-only thread never owns chain state or the async runtime.
    // Tokio stdin uses an uncancellable blocking read and can prevent shutdown.
    let reader = std::thread::spawn(move || control_input(tx));
    let mut streams = JoinSet::new();
    let mut job: Option<Job> = None;
    let mut stop_id = None;
    let mut fatal = None;
    loop {
        tokio::select! {
         command=rx.recv()=>{
          let Some(command)=command else {break};let command=match command {Ok(c)=>c,Err(e)=>{reply(Value::Null,Err(bad(&e)))?;fatal=Some(e);break;}};
          let id=command.get("id").cloned().unwrap_or(Value::Null);
          match text(&command,"op") {
           Ok("stop")=>{stop_id=Some(id);break},
           Ok("cancel")=>{if let Some(j)=&job{j.cancel.store(true,Ordering::Release);j.notify.notify_one();}reply(id,Ok(json!({"requested":job.is_some()})))?;},
           Ok("enroll")=>reply(id,text(&command,"peer_id").and_then(|s|s.parse::<PeerId>().map_err(bad)).and_then(|p|peer.enroll(p)).map(|_|json!({"enrolled":true})))?,
           Ok("status")=>{
            let result=match chain.try_lock(){Ok(c)=>status(&c,&stats),Err(std::sync::TryLockError::WouldBlock)=>Ok(json!({"busy":true,"profile":runtime_profile,"admitted_blocks":admissions.load(Ordering::Acquire),"admitted_count_is_durable_catalog_progress":true,"global_full_consensus":false})),Err(_)=>Err(bad("chain worker panicked"))};reply(id,result)?;
           },
           Ok("sync"|"follow"|"import")=>{if job.is_some(){reply(id,Err(io::Error::new(io::ErrorKind::WouldBlock,"one acquisition/import worker is already active")))?;}else{match start_job(command,peer.clone(),chain.clone(),now){Ok(j)=>job=Some(j),Err(e)=>reply(id,Err(e))?,}}},
           Ok(_)=>reply(id,Err(bad("unsupported private-node operation")))?,Err(e)=>reply(id,Err(e))?,
          }
         },
         result=async{(&mut job.as_mut().unwrap().task).await},if job.is_some()=>{let j=job.take().unwrap();reply(j.id,result.map_err(bad).and_then(|v|v))?;},
         _=streams.join_next(),if !streams.is_empty()=>{},
         event=peer.next()=>{match event? {Event::Stream{stream,..}=>{if streams.len()<32{streams.spawn(serve_stream(stream,chain.clone(),stats.clone()));}},_=>{}}}
        }
    }
    rx.close();
    drop(reader);
    streams.abort_all();
    while streams.join_next().await.is_some() {}
    if let Some(j) = job.take() {
        j.cancel.store(true, Ordering::Release);
        j.notify.notify_one();
        reply(j.id, j.task.await.map_err(bad).and_then(|v| v))?;
    }
    if let Some(id) = stop_id {
        reply(
            id,
            Ok(json!({"stopped":true,"status":status(&*lock(&chain)?,&stats)?})),
        )?;
    }
    drop(peer);
    drop(chain);
    if let Some(e) = fatal {
        return Err(e);
    }
    Ok(())
}
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> io::Result<()> {
    run().await
}
