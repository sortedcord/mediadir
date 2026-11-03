//! mediadir-server: MediaDir owns its media. Files enter through a watched ingest directory or
//! an upload endpoint, are placed into a private content-addressed store, mapped to items from
//! their file name, and served as original bytes (HTTP Range). No transcoding, ever.

mod analytics;
mod api;
mod assets;
mod config;
mod db;
mod display;
mod error;
mod frames;
mod ingest;
mod model;
mod tasks;

use api::AppState;
use config::Config;
use db::Db;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let cfg = Config::from_env();
    if let Err(e) = cfg.validate() {
        eprintln!("config: {e}");
        std::process::exit(2);
    }
    std::fs::create_dir_all(&cfg.data_dir).expect("create DATA_DIR");
    let db = match Db::open(&cfg.data_dir.join("mediadir.db")) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("database: {e}");
            std::process::exit(2);
        }
    };
    let ingest = ingest::Ingest::new(db.clone(), &cfg).expect("open ingest dir and store");
    let assets = Arc::new(assets::AssetService::new(&cfg.asset_dir, &cfg.data_dir, cfg.asset_default_width).expect("open ASSET_DIR"));
    let analytics = match analytics::Analytics::start(&cfg, db.clone()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("analytics database: {e}");
            std::process::exit(2);
        }
    };
    let tasks = tasks::Maintenance::new(db.clone(), ingest.store.clone(), assets.clone(), analytics.clone(), &cfg);
    let frames = Arc::new(frames::FrameService::new(&cfg.data_dir));
    let db_for_snap = db.clone();
    let state = Arc::new(AppState { db, data_dir: cfg.data_dir.clone(), frames, assets, ingest: ingest.clone(), tasks: tasks.clone(), analytics: analytics.clone() });

    ingest.start();
    tasks.spawn();
    analytics.spawn_snapshots(db_for_snap, ingest.clone(), tasks.clone());

    let app = api::router(state);
    let listener = tokio::net::TcpListener::bind(&cfg.bind).await.expect("bind");
    println!(
        "mediadir-server v3 listening on http://{} (ingest: {}, store: {}, assets: {}, data: {})",
        cfg.bind,
        cfg.ingest_dir.display(),
        cfg.media_dir.display(),
        cfg.asset_dir.display(),
        cfg.data_dir.display()
    );
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.expect("server");
}
