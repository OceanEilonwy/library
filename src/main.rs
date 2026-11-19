//! The Library: a home catalogue for books, music, films and kit.

mod config;
mod db;
mod digital;
mod dupes;
mod embed;
mod lookup;
mod media;
mod musicbrainz;
mod search;
mod trackmatch;
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
    /// Make a Player account, already approved (prompts for its password,
    /// or reads LIBRARY_ACCOUNT_PASSWORD).
    CreateAccount {
        username: String,
        email: String,
        /// Let the account see items marked restricted (adults only).
        #[arg(long)]
        adult: bool,
    },
    /// Change a Player account's access: child or adult.
    SetAccess { username: String, access: String },
    /// Approve a Player account that is waiting, so it can sign in.
    ApproveAccount {
        username: String,
        /// Give it adult access (otherwise it keeps the access it asked
        /// with, which is `[accounts] default_access`).
        #[arg(long)]
        adult: bool,
    },
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
        Command::CreateAccount {
            username,
            email,
            adult,
        } => {
            let cfg = Config::load(&cli.config)?;
            create_account(&cfg, &username, &email, adult)
        }
        Command::SetAccess { username, access } => {
            let cfg = Config::load(&cli.config)?;
            set_access(&cfg, &username, &access)
        }
        Command::ApproveAccount { username, adult } => {
            let cfg = Config::load(&cli.config)?;
            approve_account(&cfg, &username, adult)
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

/// Makes an account as the librarian: it is approved straight away.
fn make_account(
    db: &Db,
    username: &str,
    email: &str,
    password: &str,
    adult: bool,
) -> Result<db::accounts::Account> {
    use db::accounts::{self, Access, Status};
    let bad = accounts::validate(username, email, password);
    if !bad.is_empty() {
        anyhow::bail!("{bad}");
    }
    let hash = web::auth::hash_password(password)?;
    let access = if adult { Access::Adult } else { Access::Child };
    db.write_blocking(|c| accounts::create(c, username, email, &hash, access, Status::Approved))
}

fn create_account(cfg: &Config, username: &str, email: &str, adult: bool) -> Result<()> {
    let password = match std::env::var("LIBRARY_ACCOUNT_PASSWORD") {
        Ok(p) if !p.is_empty() => p,
        _ => {
            let pw = rpassword::prompt_password(format!("Password for {username}: "))?;
            let again = rpassword::prompt_password("Again: ")?;
            if pw != again {
                anyhow::bail!("The two passwords differ.");
            }
            pw
        }
    };
    let db = Db::open(&cfg.db_path())?;
    let account = make_account(&db, username, email, &password, adult)?;
    println!(
        "Made the account {} ({}), access: {}.",
        account.username,
        account.email,
        account.access.as_str()
    );
    Ok(())
}

fn set_access(cfg: &Config, username: &str, access: &str) -> Result<()> {
    use db::accounts::{self, Access};
    let access =
        Access::parse(access).ok_or_else(|| anyhow::anyhow!("Access should be child or adult."))?;
    let db = Db::open(&cfg.db_path())?;
    let account = db
        .read_blocking(|c| accounts::by_username(c, username))?
        .ok_or_else(|| anyhow::anyhow!("There is no account called {username}."))?;
    db.write_blocking(|c| accounts::set_access(c, account.id, access))?;
    println!("{} now has {} access.", account.username, access.as_str());
    Ok(())
}

/// Approves a waiting account; false if it was approved already.
fn approve(db: &Db, username: &str, adult: bool) -> Result<(db::accounts::Account, bool)> {
    use db::accounts::{self, Access};
    let account = db
        .read_blocking(|c| accounts::by_username(c, username))?
        .ok_or_else(|| anyhow::anyhow!("There is no account called {username}."))?;
    if account.approved() {
        return Ok((account, false));
    }
    let access = if adult { Access::Adult } else { account.access };
    let id = account.id;
    db.write_blocking(|c| accounts::approve(c, id, access))?;
    let account = db
        .read_blocking(|c| accounts::get(c, id))?
        .ok_or_else(|| anyhow::anyhow!("The account {username} vanished."))?;
    Ok((account, true))
}

fn approve_account(cfg: &Config, username: &str, adult: bool) -> Result<()> {
    let db = Db::open(&cfg.db_path())?;
    let (account, changed) = approve(&db, username, adult)?;
    if changed {
        println!(
            "Approved {} with {} access. They can sign in now.",
            account.username,
            account.access.as_str()
        );
    } else {
        println!("{} is already approved.", account.username);
    }
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
    std::fs::create_dir_all(cfg.media_dir())?;
    // Uploads a previous run was killed in the middle of.
    match web::studio::sweep_partial_uploads(&cfg.media_dir()) {
        Ok(0) => {}
        Ok(n) => tracing::info!(files = n, "removed unfinished uploads"),
        Err(e) => tracing::warn!(error = %e, "could not clear unfinished uploads"),
    }
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
        throttles: Arc::new(web::accounts::Throttles::default()),
        rip: Arc::new(web::rip::RipState::default()),
    };
    let app = web::router(state);
    let listener = tokio::net::TcpListener::bind(cfg.listen)
        .await
        .with_context(|| format!("listening on {}", cfg.listen))?;
    tracing::info!(addr = %cfg.listen, "library is open");
    // Connection info gives the rate limits each client's address.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(web::shutdown_signal())
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::accounts::{self, Access, Status};

    #[test]
    fn cli_accounts_are_approved() {
        let (db, _dir) = db::testing::temp_db();
        let a = make_account(&db, "admin", "admin@example.org", "long enough", true).unwrap();
        assert!(a.approved() && a.access == Access::Adult);
        let k = make_account(&db, "kid", "kid@example.org", "long enough", false).unwrap();
        assert!(k.approved() && k.access == Access::Child);
        assert!(make_account(&db, "x", "nope", "short", false).is_err());
    }

    #[test]
    fn cli_approves_waiting_accounts() {
        let (db, _dir) = db::testing::temp_db();
        db.write_blocking(|c| {
            accounts::create(
                c,
                "newbie",
                "n@example.org",
                "h",
                Access::Child,
                Status::Pending,
            )?;
            accounts::create(
                c,
                "other",
                "o@example.org",
                "h",
                Access::Child,
                Status::Pending,
            )?;
            Ok(())
        })
        .unwrap();
        let (a, changed) = approve(&db, "newbie", false).unwrap();
        assert!(changed && a.approved() && a.access == Access::Child);
        let (_, changed) = approve(&db, "NEWBIE", true).unwrap();
        assert!(!changed, "already approved");
        let (o, _) = approve(&db, "other", true).unwrap();
        assert!(o.approved() && o.access == Access::Adult);
        assert!(approve(&db, "nobody", false).is_err());
    }
}
