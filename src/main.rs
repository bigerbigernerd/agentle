use agentle::store::Store;
use agentle::web::{self, App, API_RPM, PAGE_RPM, TRUST_PROXY};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

fn env(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let addr = env("AGENTLE_ADDR", "127.0.0.1:18990");
    let data = PathBuf::from(env("AGENTLE_DATA", "./data"));
    let root = PathBuf::from(env("AGENTLE_ROOT", "."));
    let base = env("AGENTLE_BASE", "https://agentle.pocketplay.win");
    let min_free_gb: u64 = env("AGENTLE_MIN_FREE_GB", "8").parse().unwrap_or(8);
    TRUST_PROXY.store(env("AGENTLE_TRUST_PROXY", "0") == "1", Ordering::Relaxed);
    PAGE_RPM.store(env("AGENTLE_PAGE_RPM", "600").parse().unwrap_or(600), Ordering::Relaxed);
    API_RPM.store(env("AGENTLE_API_RPM", "180").parse().unwrap_or(180), Ordering::Relaxed);

    let store = match Store::open(&data, &base, min_free_gb * 1024 * 1024 * 1024) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("agentle: cannot open data dir {}: {e}", data.display());
            std::process::exit(1);
        }
    };

    if let Ok(n) = env("AGENTLE_REG_PER_IP", "").parse::<i64>() {
        store.set_register_limit(n);
    }

    if args.first().map(|s| s.as_str()) == Some("admin") {
        let out = if args.get(1).map(|s| s.as_str()) == Some("seed") {
            agentle::seed::run(&store, args.get(2).map(|s| s.as_str()).unwrap_or("house/opening.json"))
        } else {
            store.admin(&args[1..])
        };
        match out {
            Ok(out) => println!("{out}"),
            Err(e) => {
                eprintln!("{}: {}", e.code, e.message);
                std::process::exit(2);
            }
        }
        return;
    }

    let app = match App::new(store, &root) {
        Ok(a) => Arc::new(a),
        Err(e) => {
            eprintln!("agentle: {e}");
            std::process::exit(1);
        }
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(16)
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move {
        let upkeep = app.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(300));
            loop {
                tick.tick().await;
                let a = upkeep.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if let Err(e) = a.store.maintain() {
                        eprintln!("[agentle] upkeep failed: {}", e.message);
                    }
                    a.sweep_limiter();
                })
                .await;
            }
        });
        let listener = tokio::net::TcpListener::bind(&addr).await.unwrap_or_else(|e| {
            eprintln!("agentle: cannot bind {addr}: {e}");
            std::process::exit(1);
        });
        eprintln!("agentle listening on http://{addr} (public base {base})");
        let svc = web::router(app).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, svc)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
            .expect("server");
    });
}
