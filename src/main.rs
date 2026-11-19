//! The Library: a home catalogue for books, music, films and kit.

mod config;
mod db;
mod dupes;
mod embed;
mod lookup;
mod media;
mod search;
mod util;
mod views;
mod web;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use config::Config;
use db::Db;
use embed::Semantic;

#[derive(Parser)]
#[command(version, about = "A home library catalogue")]
struct Cli {
    /// Path to the config file.
    #[arg(
        short,
        long,
        env = "LIBRARY_CONFIG",
        default_value = "library.toml",
        global = true
    )]
    config: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the web server (the default).
    Serve {
        /// Address to listen on, overriding the config file.
        #[arg(long)]
        listen: Option<std::net::SocketAddr>,
        /// Data directory, overriding the config file.
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// Hash an admin password for the config file.
    HashPassword {
        /// Read the password from standard input instead of prompting.
        #[arg(long)]
        stdin: bool,
    },
    /// Rebuild the full-text index and re-embed every item.
    Reindex,
    /// Write a consistent copy of the database to a file.
    Backup { dest: PathBuf },
    /// Download the embedding model now (instead of on first start).
    FetchModel,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "library=info,tower_http=warn".into()),
        )
        .init();
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Serve {
        listen: None,
        data_dir: None,
    }) {
        Command::HashPassword { stdin } => hash_password(stdin),
        Command::Serve { listen, data_dir } => {
            let mut cfg = Config::load(&cli.config)?;
            if let Some(l) = listen {
                cfg.listen = l;
            }
            if let Some(d) = data_dir {
                cfg.data_dir = d;
            }
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(serve(cfg))
        }
        Command::Reindex => {
            let cfg = Config::load(&cli.config)?;
            reindex(cfg)
        }
        Command::Backup { dest } => {
            let cfg = Config::load(&cli.config)?;
            let db = Db::open(&cfg.db_path())?;
            db.read_blocking(|c| db::backup(c, &dest))?;
            println!("Backed up to {}", dest.display());
            Ok(())
        }
        Command::FetchModel => {
            let cfg = Config::load(&cli.config)?;
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    let client = reqwest::Client::builder()
                        .user_agent("home-library")
                        .build()?;
                    let dir = embed::bert::model_dir(&cfg.models_dir(), &cfg.semantic.model);
                    embed::bert::ensure_downloaded(
                        &client,
                        &cfg.semantic.hub_url,
                        &cfg.semantic.model,
                        &dir,
                    )
                    .await?;
                    println!("Model ready in {}", dir.display());
                    Ok(())
                })
        }
    }
}

fn hash_password(stdin: bool) -> Result<()> {
    let pw = if stdin {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        line.trim_end_matches(['\r', '\n']).to_owned()
    } else {
        rpassword::prompt_password("Admin password: ")?
    };
    if pw.chars().count() < 8 {
        anyhow::bail!("Use at least 8 characters.");
    }
    if !stdin {
        let again = rpassword::prompt_password("Again: ")?;
        if pw != again {
            anyhow::bail!("The two passwords differ.");
        }
    }
    let hash = web::auth::hash_password(&pw)?;
    println!("\nAdd this line to your config file:\n\nadmin_password_hash = \"{hash}\"\n");
    Ok(())
}

fn reindex(cfg: Config) -> Result<()> {
    let db = Db::open(&cfg.db_path())?;
    let n = db.write_blocking(|c| {
        let ids: Vec<i64> = c
            .prepare("SELECT id FROM items")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let tx = c.transaction()?;
        tx.execute("DELETE FROM items_fts", [])?;
        for id in &ids {
            db::items::sync_fts(&tx, *id)?;
        }
        tx.execute("UPDATE items SET embed_hash = NULL", [])?;
        tx.commit()?;
        Ok(ids.len())
    })?;
    println!("Rebuilt the full-text index for {n} items.");
    if cfg.semantic.enabled {
        let dir = embed::bert::model_dir(&cfg.models_dir(), &cfg.semantic.model);
        let emb = embed::bert::BertEmbedder::load(&cfg.semantic.model, &dir)
            .context("loading the model (run `library fetch-model` first)")?;
        let done = embed::index_all_blocking(&db, &emb)?;
        println!("Embedded {done} items.");
    }
    Ok(())
}

async fn serve(cfg: Config) -> Result<()> {
    std::fs::create_dir_all(&cfg.data_dir)
        .with_context(|| format!("creating {}", cfg.data_dir.display()))?;
    std::fs::create_dir_all(cfg.photos_dir())?;
    let db = Db::open(&cfg.db_path())?;
    let key = web::auth::Auth::load_or_create_key(&cfg.secret_path())?;
    let password_hash = match (&cfg.admin_password_hash, &cfg.admin_password) {
        (Some(h), _) => Some(h.clone()),
        (None, Some(plain)) => Some(web::auth::hash_password(plain)?),
        (None, None) => {
            tracing::warn!(
                "no admin password configured: editing is disabled (run `library hash-password`)"
            );
            None
        }
    };
    let auth = web::auth::Auth::new(key, password_hash);
    let lookups = lookup::Lookups::new(cfg.lookup.clone())?;
    let semantic = Semantic::disabled();
    let cfg = Arc::new(cfg);

    if cfg.semantic.enabled {
        // Load (downloading on first run) the model in the background so the
        // site is usable at once; meaning search switches on when it's ready.
        let (cfg2, sem2, client) = (cfg.clone(), semantic.clone(), lookups.client().clone());
        tokio::spawn(async move {
            let dir = embed::bert::model_dir(&cfg2.models_dir(), &cfg2.semantic.model);
            if let Err(e) = embed::bert::ensure_downloaded(
                &client,
                &cfg2.semantic.hub_url,
                &cfg2.semantic.model,
                &dir,
            )
            .await
            {
                tracing::error!(error = %e, "could not download the embedding model; meaning search is off");
                return;
            }
            let model = cfg2.semantic.model.clone();
            match tokio::task::spawn_blocking(move || embed::bert::BertEmbedder::load(&model, &dir))
                .await
            {
                Ok(Ok(e)) => {
                    tracing::info!(model = %cfg2.semantic.model, "embedding model loaded");
                    sem2.install(Arc::new(e));
                }
                Ok(Err(e)) => tracing::error!(error = %e, "could not load the embedding model"),
                Err(e) => tracing::error!(error = %e, "model loading task failed"),
            }
        });
        tokio::spawn(semantic.clone().run_indexer(db.clone()));
    }

    let state = web::AppState {
        art: Arc::new(web::ArtCache::new(cfg.art_dir())),
        cfg: cfg.clone(),
        db,
        semantic,
        lookups,
        candidates: Arc::new(lookup::CandidateCache::default()),
        auth: Arc::new(auth),
    };
    let app = web::router(state);
    let listener = tokio::net::TcpListener::bind(cfg.listen)
        .await
        .with_context(|| format!("listening on {}", cfg.listen))?;
    tracing::info!(addr = %cfg.listen, "library is open");
    axum::serve(listener, app)
        .with_graceful_shutdown(web::shutdown_signal())
        .await?;
    Ok(())
}
