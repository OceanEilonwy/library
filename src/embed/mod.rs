//! Sentence embeddings for meaning-based search.
//!
//! An [`Embedder`] turns text into a unit-length vector; similar meanings end
//! up close together. Item vectors live in the sqlite-vec table
//! `item_vectors` (rowid = item id) and a background worker keeps them up to
//! date as items are added or edited.

pub mod bert;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use crate::db::{self, Db, items};

pub trait Embedder: Send + Sync + 'static {
    /// Identifies the model; changing it rebuilds every vector.
    fn model_id(&self) -> &str;
    fn dim(&self) -> usize;
    /// Embeds a search query (some models want a query instruction).
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
    /// Embeds catalogue entries.
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    /// Cosine similarities that count as related; these depend on the model.
    fn thresholds(&self) -> Thresholds {
        Thresholds::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// A meaning-only match joins the main results.
    pub main: f32,
    /// A meaning-only match is offered under "See also".
    pub see_also: f32,
    /// An explicit `idea:` term is satisfied.
    pub idea: f32,
}

impl Default for Thresholds {
    /// Tuned for bge-small-en-v1.5, whose unrelated pairs sit around 0.5.
    fn default() -> Self {
        Self {
            main: 0.74,
            see_also: 0.66,
            idea: 0.66,
        }
    }
}

/// The meaning-search engine as the rest of the app sees it: maybe an
/// embedder (it loads in the background), and whether vectors are ready.
#[derive(Clone, Default)]
pub struct Semantic {
    inner: Arc<SemanticInner>,
}

#[derive(Default)]
struct SemanticInner {
    embedder: std::sync::OnceLock<Arc<dyn Embedder>>,
    wake: Notify,
    indexing: AtomicBool,
}

impl Semantic {
    pub fn disabled() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn with(embedder: Arc<dyn Embedder>) -> Self {
        let s = Self::default();
        let _ = s.inner.embedder.set(embedder);
        s
    }

    pub fn install(&self, embedder: Arc<dyn Embedder>) {
        let _ = self.inner.embedder.set(embedder);
        self.inner.wake.notify_one();
    }

    pub fn embedder(&self) -> Option<Arc<dyn Embedder>> {
        self.inner.embedder.get().cloned()
    }

    pub fn thresholds(&self) -> Thresholds {
        self.embedder().map(|e| e.thresholds()).unwrap_or_default()
    }

    /// Tells the indexer that items changed.
    pub fn poke(&self) {
        self.inner.wake.notify_one();
    }

    pub fn is_indexing(&self) -> bool {
        self.inner.indexing.load(Ordering::Relaxed)
    }

    /// Embeds a query on a blocking thread.
    pub async fn embed_query(&self, text: &str) -> Option<Vec<f32>> {
        let emb = self.embedder()?;
        let text = text.to_owned();
        match tokio::task::spawn_blocking(move || emb.embed_query(&text)).await {
            Ok(Ok(v)) => Some(v),
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "query embedding failed");
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "query embedding task failed");
                None
            }
        }
    }

    /// Runs forever, embedding items whose text changed. Woken by [`poke`].
    pub async fn run_indexer(self, db: Db) {
        loop {
            if let Some(emb) = self.embedder() {
                self.inner.indexing.store(true, Ordering::Relaxed);
                let res = index_pending(&db, emb).await;
                self.inner.indexing.store(false, Ordering::Relaxed);
                match res {
                    Ok(0) => {}
                    Ok(n) => {
                        tracing::info!(items = n, "embedded items");
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "embedding pass failed; retrying shortly");
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        continue;
                    }
                }
            }
            // Wait for a change, with an occasional sweep in case a wake-up was missed.
            let _ =
                tokio::time::timeout(Duration::from_secs(300), self.inner.wake.notified()).await;
        }
    }
}

const BATCH: usize = 16;

/// Embeds up to one batch of items that need it. Returns how many were done.
pub async fn index_pending(db: &Db, emb: Arc<dyn Embedder>) -> Result<usize> {
    let (model, dim) = (emb.model_id().to_owned(), emb.dim());
    db.write(move |c| db::ensure_vector_table(c, &model, dim))
        .await?;
    let batch: Vec<(i64, String)> = db
        .read(|c| {
            let ids = items::needing_embedding(c, BATCH)?;
            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(text) = items::embedding_text(c, id)? {
                    out.push((id, text));
                }
            }
            Ok(out)
        })
        .await?;
    if batch.is_empty() {
        return Ok(0);
    }
    let texts: Vec<String> = batch.iter().map(|(_, t)| t.clone()).collect();
    let emb2 = emb.clone();
    let vectors = tokio::task::spawn_blocking(move || emb2.embed_documents(&texts)).await??;
    let n = batch.len();
    db.write(move |c| {
        let tx = c.transaction()?;
        for ((id, text), v) in batch.iter().zip(vectors) {
            store_vector(&tx, *id, &v, &text_hash(text))?;
        }
        tx.commit()?;
        Ok(())
    })
    .await?;
    Ok(n)
}

/// Embeds every pending item synchronously (CLI `reindex`, tests).
pub fn index_all_blocking(db: &Db, emb: &dyn Embedder) -> Result<usize> {
    db.write_blocking(|c| db::ensure_vector_table(c, emb.model_id(), emb.dim()))?;
    let mut total = 0;
    loop {
        let batch: Vec<(i64, String)> = db.read_blocking(|c| {
            let mut out = Vec::new();
            for id in items::needing_embedding(c, BATCH)? {
                if let Some(t) = items::embedding_text(c, id)? {
                    out.push((id, t));
                }
            }
            Ok(out)
        })?;
        if batch.is_empty() {
            return Ok(total);
        }
        let texts: Vec<String> = batch.iter().map(|(_, t)| t.clone()).collect();
        let vectors = emb.embed_documents(&texts)?;
        db.write_blocking(|c| {
            let tx = c.transaction()?;
            for ((id, text), v) in batch.iter().zip(&vectors) {
                store_vector(&tx, *id, v, &text_hash(text))?;
            }
            tx.commit()?;
            Ok(())
        })?;
        total += batch.len();
    }
}

fn text_hash(text: &str) -> String {
    let d = Sha256::digest(text.as_bytes());
    d.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

pub fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn store_vector(conn: &Connection, id: i64, v: &[f32], hash: &str) -> Result<()> {
    conn.execute("DELETE FROM item_vectors WHERE rowid = ?1", [id])?;
    conn.execute(
        "INSERT INTO item_vectors (rowid, embedding) VALUES (?1, ?2)",
        params![id, to_blob(v)],
    )?;
    conn.execute(
        "UPDATE items SET embed_hash = ?2 WHERE id = ?1",
        params![id, hash],
    )?;
    Ok(())
}

/// Nearest items to `v`, as (item id, cosine similarity), best first.
pub fn neighbours(conn: &Connection, v: &[f32], k: usize) -> Result<Vec<(i64, f32)>> {
    if !db::vector_table_exists(conn)? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare_cached(
        "SELECT rowid, distance FROM item_vectors WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance",
    )?;
    let rows = stmt.query_map(params![to_blob(v), k as i64], |r| {
        Ok((r.get::<_, i64>(0)?, 1.0 - r.get::<_, f64>(1)? as f32))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The stored vector for an item (for "more like this").
pub fn item_vector(conn: &Connection, id: i64) -> Result<Option<Vec<f32>>> {
    if !db::vector_table_exists(conn)? {
        return Ok(None);
    }
    use rusqlite::OptionalExtension;
    let blob: Option<Vec<u8>> = conn
        .query_row(
            "SELECT embedding FROM item_vectors WHERE rowid = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(blob.map(|b| {
        b.as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }))
}

pub fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

/// A deterministic toy embedder for tests: words in the same "concept" group
/// land on the same axis, so "ocean" is close to "sea" and "whale".
#[cfg(test)]
pub mod fake {
    use super::*;

    pub struct FakeEmbedder;

    const GROUPS: &[&[&str]] = &[
        &[
            "sea", "ocean", "whale", "whaling", "fish", "water", "sailing", "voyage", "swimming",
            "island", "islands",
        ],
        &[
            "wizard", "magic", "dragon", "dragons", "fantasy", "quest", "spell",
        ],
        &["tent", "camping", "stove", "hiking", "outdoors", "rucksack"],
        &["flying", "kite", "wind", "aircraft", "flight"],
    ];
    const DIM: usize = 32;

    fn axis(word: &str) -> usize {
        for (i, g) in GROUPS.iter().enumerate() {
            if g.contains(&word) {
                return i;
            }
        }
        let h = word
            .bytes()
            .fold(7usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize));
        GROUPS.len() + h % (DIM - GROUPS.len())
    }

    fn embed(text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; DIM];
        for w in crate::util::normalize(text)
            .split(' ')
            .filter(|w| w.len() > 2)
        {
            v[axis(w)] += 1.0;
        }
        normalize(&mut v);
        v
    }

    impl Embedder for FakeEmbedder {
        fn model_id(&self) -> &str {
            "fake"
        }
        fn dim(&self) -> usize {
            DIM
        }
        fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            Ok(embed(text))
        }
        fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|t| embed(t)).collect())
        }
        fn thresholds(&self) -> Thresholds {
            Thresholds {
                main: 0.4,
                see_also: 0.2,
                idea: 0.25,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeEmbedder;
    use super::*;
    use crate::db::items::{create, tests::input};
    use crate::db::testing::temp_db;

    #[test]
    fn index_and_query_neighbours() {
        let (db, _d) = temp_db();
        let (moby, tent) = db
            .write_blocking(|c| {
                let mut m = input(c, "Moby-Dick", "Herman Melville");
                m.description = "whaling voyage at sea".into();
                let mut t = input(c, "Tent", "MSR");
                t.description = "camping hiking".into();
                Ok((create(c, &m)?, create(c, &t)?))
            })
            .unwrap();
        let n = index_all_blocking(&db, &FakeEmbedder).unwrap();
        assert_eq!(n, 2);
        assert_eq!(
            index_all_blocking(&db, &FakeEmbedder).unwrap(),
            0,
            "nothing left to do"
        );
        let q = FakeEmbedder.embed_query("ocean").unwrap();
        let near = db.read_blocking(|c| neighbours(c, &q, 2)).unwrap();
        assert_eq!(near[0].0, moby);
        assert!(near[0].1 > near[1].1);
        assert_eq!(near[1].0, tent);
        let v = db.read_blocking(|c| item_vector(c, moby)).unwrap().unwrap();
        assert_eq!(v.len(), FakeEmbedder.dim());
    }

    #[test]
    fn editing_an_item_queues_it_again() {
        let (db, _d) = temp_db();
        let id = db
            .write_blocking(|c| create(c, &input(c, "Kite", "")))
            .unwrap();
        index_all_blocking(&db, &FakeEmbedder).unwrap();
        db.write_blocking(|c| crate::db::items::update(c, id, &input(c, "Kite, diamond", "")))
            .unwrap();
        assert_eq!(index_all_blocking(&db, &FakeEmbedder).unwrap(), 1);
    }

    #[tokio::test]
    async fn background_indexer_picks_up_new_items() {
        let (db, _d) = temp_db();
        let sem = Semantic::with(Arc::new(FakeEmbedder));
        tokio::spawn(sem.clone().run_indexer(db.clone()));
        db.write(|c| create(c, &input(c, "Moby-Dick", "")))
            .await
            .unwrap();
        sem.poke();
        for _ in 0..100 {
            let pending = db.read(|c| items::needing_embedding(c, 10)).await.unwrap();
            if pending.is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("indexer never embedded the item");
    }
}
