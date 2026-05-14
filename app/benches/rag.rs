//! RAG subsystem benchmarks (§9.2).
//!
//! Run with: `cargo bench --bench rag`
//!
//! Pass thresholds per TDD §9.2:
//!   bench_default_redactor_1kb  < 1ms P99
//!   bench_chunk_10kb            < 5ms P99
//!   bench_knn_10k_rows          < 10ms P99
//!   bench_knn_100k_rows         < 100ms P99 (aspirational; does not block release)

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use diesel::prelude::*;
use diesel_migrations::MigrationHarness;
use tempfile::TempDir;
use warp::rag::{
    chunk::{chunk_text, ChunkConfig},
    redact::{DefaultRedactor, Redactor},
    store::{init_sqlite_vec, VectorStore},
};

const EMBEDDING_DIM: usize = 768;

fn fake_embedding(seed: usize) -> Vec<f32> {
    (0..EMBEDDING_DIM)
        .map(|i| (((seed.wrapping_mul(6364136223846793005).wrapping_add(i)) % 10000) as f32)
            / 10000.0)
        .collect()
}

/// Create a temp SQLite DB with all migrations applied, seeded with `row_count`
/// command-block chunks. Returns the `TempDir` (must be kept alive) and a
/// `VectorStore` bound to that DB.
fn setup_bench_db(row_count: usize) -> (TempDir, VectorStore) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("bench.db");

    // Register sqlite-vec BEFORE the first connection so that migrations can
    // create the vec0 virtual tables.
    init_sqlite_vec();

    let db_url = db_path.to_str().unwrap();
    let mut conn = SqliteConnection::establish(db_url).unwrap();
    conn.run_pending_migrations(persistence::MIGRATIONS)
        .unwrap();

    let store = VectorStore::new(&db_path).unwrap();

    let batch = 100usize;
    for start in (0..row_count).step_by(batch) {
        let end = (start + batch).min(row_count);
        let texts: Vec<String> = (start..end)
            .map(|i| format!("bench chunk text for block {i}"))
            .collect();
        let chunks: Vec<(i32, &str)> = texts.iter().enumerate().map(|(i, s)| (i as i32, s.as_str())).collect();
        let embeddings: Vec<Vec<f32>> = (start..end).map(fake_embedding).collect();
        store
            .insert_command_block_chunks(&format!("bench-block-{start}"), &chunks, &embeddings)
            .unwrap();
    }

    (dir, store)
}

fn bench_default_redactor_1kb(c: &mut Criterion) {
    let payload = format!(
        "API_KEY=sk-abc123def456ghi789 password=supersecret123\n\
         export AWS_SECRET=aws-secret-key-1234567890abcdef\n\
         {}",
        "x".repeat(900)
    );
    let redactor = DefaultRedactor::new(&[]).unwrap();
    c.bench_function("rag::redact::bench_default_redactor_1kb", |b| {
        b.iter(|| redactor.redact(black_box(&payload)))
    });
}

fn bench_chunk_10kb(c: &mut Criterion) {
    let paragraph = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, \
        sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. ";
    // ~128 chars per repetition × 80 ≈ 10KB; separated by double newlines so
    // the chunker sees paragraph boundaries.
    let text: String = std::iter::repeat(paragraph)
        .take(80)
        .collect::<Vec<_>>()
        .join("\n\n");
    let config = ChunkConfig::default();
    c.bench_function("rag::chunk::bench_chunk_10kb", |b| {
        b.iter(|| chunk_text(black_box(&text), black_box(&config)))
    });
}

fn bench_knn_10k_rows(c: &mut Criterion) {
    let (_dir, store) = setup_bench_db(10_000);
    let query = fake_embedding(999);
    c.bench_function("rag::store::bench_knn_10k_rows", |b| {
        b.iter(|| store.knn_command_blocks(black_box(&query), black_box(5)).unwrap())
    });
}

fn bench_knn_100k_rows(c: &mut Criterion) {
    let (_dir, store) = setup_bench_db(100_000);
    let query = fake_embedding(999);
    c.bench_function("rag::store::bench_knn_100k_rows", |b| {
        b.iter(|| store.knn_command_blocks(black_box(&query), black_box(5)).unwrap())
    });
}

criterion_group!(
    name = redact_chunk;
    config = Criterion::default();
    targets = bench_default_redactor_1kb, bench_chunk_10kb
);
criterion_group!(
    name = knn_10k;
    config = Criterion::default();
    targets = bench_knn_10k_rows
);
criterion_group!(
    name = knn_100k;
    config = Criterion::default().sample_size(10);
    targets = bench_knn_100k_rows
);
criterion_main!(redact_chunk, knn_10k, knn_100k);
