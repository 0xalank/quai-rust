use quai_rpc::{router, App, Config};
use serde_json::json;

fn parse_args() -> Result<Config, String> {
    let mut config = Config::default();
    if !cfg!(feature = "reference-gateway") {
        config.mode = "snapshot".into();
    }
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            println!("quai-rpc [--mode fixture|gateway|hybrid|snapshot] [--snapshot FILE] [--native-canonical-reads true] [--follow true --poll-ms 1000 --window-blocks 128] [--upstream http://127.0.0.1:8545] [--listen 127.0.0.1:8547]\n  --workers 2 --batch-concurrency 1 --encoding value|raw\n  --max-inflight 128 --max-batch 128 --max-body-bytes 1048576\n  --max-response-bytes 10485760 --timeout-ms 5000");
            std::process::exit(0);
        }
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {arg}"))?;
        match arg.as_str() {
            "--listen" => config.listen = value,
            "--native-canonical-reads" => {
                config.native_canonical_reads =
                    value.parse().map_err(|_| "invalid native reads boolean")?
            }
            "--follow" => config.follow = value.parse().map_err(|_| "invalid follow boolean")?,
            "--poll-ms" => config.poll_ms = value.parse().map_err(|_| "invalid poll interval")?,
            "--window-blocks" => {
                config.window_blocks = value.parse().map_err(|_| "invalid window")?
            }
            "--snapshot" => config.snapshot = Some(value),
            "--mode" => config.mode = value,
            "--upstream" => config.upstream = Some(value),
            "--encoding" => config.encoding = value,
            "--workers" => config.workers = value.parse().map_err(|_| "invalid workers")?,
            "--batch-concurrency" => {
                config.batch_concurrency = value.parse().map_err(|_| "invalid batch concurrency")?
            }
            "--max-inflight" => {
                config.max_inflight = value.parse().map_err(|_| "invalid inflight limit")?
            }
            "--max-batch" => config.max_batch = value.parse().map_err(|_| "invalid batch limit")?,
            "--max-body-bytes" => {
                config.max_body_bytes = value.parse().map_err(|_| "invalid body limit")?
            }
            "--max-response-bytes" => {
                config.max_response_bytes = value.parse().map_err(|_| "invalid response limit")?
            }
            "--timeout-ms" => config.timeout_ms = value.parse().map_err(|_| "invalid timeout")?,
            _ => return Err(format!("unknown option: {arg}")),
        }
    }
    config.validate()?;
    Ok(config)
}

fn main() {
    let config = match parse_args() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2)
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.workers)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("runtime: {error}");
            std::process::exit(1)
        }
    };
    if let Err(error)=runtime.block_on(async move {
        #[cfg(feature="reference-gateway")]
        let follower = if config.follow {Some(quai_rpc::ingest::Follower::open(&config).await?)} else {None};
        let app=App::new(config.clone())?;
        #[cfg(feature="reference-gateway")]
        if !config.follow {app.verify_store_upstream().await?;}
        #[cfg(feature="reference-gateway")]
        if let Some(follower)=follower {tokio::spawn(follower.run(app.clone()));}
        #[cfg(unix)]
        if config.snapshot.is_some() {
            let reload_app = app.clone();
            tokio::spawn(async move {
                let mut signals = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
                    Ok(signals) => signals,
                    Err(error) => {eprintln!("SIGHUP reload disabled: {error}");return;}
                };
                while signals.recv().await.is_some() {
                    if reload_app.config.follow {eprintln!("follower owns checkpoint; manual reload ignored");continue;}
                    let app = reload_app.clone();
                    match tokio::task::spawn_blocking(move || app.reload_snapshot()).await {
                        Ok(Ok(())) => (),
                        Ok(Err(error)) => eprintln!("snapshot reload rejected: {error}"),
                        Err(error) => eprintln!("snapshot reload task failed: {error}"),
                    }
                }
            });
        }
        let listener=tokio::net::TcpListener::bind(&config.listen).await.map_err(|e|e.to_string())?;
        let address=listener.local_addr().map_err(|e|e.to_string())?;
        println!("{}",json!({"ready":true,"address":address.to_string(),"mode":config.mode,"version":quai_rpc::VERSION}));
        axum::serve(listener,router(app)).with_graceful_shutdown(async {let _=tokio::signal::ctrl_c().await;})
            .await.map_err(|e|e.to_string())
    }) {eprintln!("{error}");std::process::exit(1)}
}
