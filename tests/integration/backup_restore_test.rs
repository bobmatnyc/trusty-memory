//! Round-trip integration test for `trusty-memory backup` / `restore`.
//!
//! Why: Backup/restore is the disaster-recovery story for palace data; if the
//! archive doesn't faithfully reconstruct a working palace, users lose
//! memories without warning.
//! What: Builds a palace under a tempdir-backed data_root, persists a few
//! drawers + a KG triple, runs `backup_one` to produce a `.tar.gz`,
//! deletes the palace dir, runs `restore_to_root`, and verifies the palace
//! data is restored (palace.json + kg.db with the same triple).
//! Test: `cargo test --test integration_tests backup_restore`

use std::path::Path;
use tempfile::tempdir;

use trusty_memory::cli::backup::{
    backup_one, default_output_filename, restore_to_root, RestoreArgs,
};
use trusty_memory::cli::output::OutputConfig;
use trusty_memory_core::store::kg::{KnowledgeGraph, Triple};
use trusty_memory_core::{
    palace::{Palace, PalaceId},
    PalaceRegistry,
};

fn quiet_out() -> OutputConfig {
    OutputConfig {
        json: false,
        quiet: true,
        no_color: true,
    }
}

async fn count_kg_triples(palace_dir: &Path) -> usize {
    let kg = KnowledgeGraph::open(&palace_dir.join("kg.db")).expect("open kg");
    kg.query_active("Rust").await.unwrap_or_default().len()
}

#[tokio::test]
async fn backup_then_restore_round_trip() {
    let workspace = tempdir().expect("workspace tmp");
    let data_root = workspace.path().join("data");
    std::fs::create_dir_all(&data_root).unwrap();

    // 1. Create a palace + add a KG triple.
    let palace_id = PalaceId::new("rt-test");
    let palace = Palace {
        id: palace_id.clone(),
        name: "rt-test".to_string(),
        description: Some("round-trip test".to_string()),
        created_at: chrono::Utc::now(),
        data_dir: data_root.join("rt-test"),
    };
    let reg = PalaceRegistry::new();
    let _handle = reg.create_palace(&data_root, palace).expect("create");

    // Open the KG directly and assert a triple — this mimics what
    // `kg_assert` does and gives us something restoreable.
    {
        let kg = KnowledgeGraph::open(&data_root.join("rt-test/kg.db")).expect("open kg");
        let triple = Triple {
            subject: "Rust".to_string(),
            predicate: "uses".to_string(),
            object: "HNSW".to_string(),
            valid_from: chrono::Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: Some("integration-test".to_string()),
        };
        kg.assert(triple).await.expect("assert triple");
    }

    // Sanity: the palace dir exists and the KG holds our triple.
    assert!(data_root.join("rt-test/palace.json").exists());
    assert!(data_root.join("rt-test/kg.db").exists());
    assert_eq!(count_kg_triples(&data_root.join("rt-test")).await, 1);

    // 2. Back up to a tempdir.
    let archive_dir = workspace.path().join("archives");
    std::fs::create_dir_all(&archive_dir).unwrap();
    let archive_path = archive_dir.join(default_output_filename("rt-test", "20260509"));
    backup_one(&data_root, "rt-test", &archive_path, &quiet_out()).expect("backup");
    assert!(archive_path.exists(), "archive should exist after backup");
    let archive_size = std::fs::metadata(&archive_path).unwrap().len();
    assert!(archive_size > 0, "archive should not be empty");

    // 3. Drop the palace from the registry then delete the on-disk dir.
    drop(reg); // releases SQLite connection pool
               // Give SQLite a beat to release file handles on platforms that need it.
    let palace_path = data_root.join("rt-test");
    std::fs::remove_dir_all(&palace_path).expect("delete palace dir");
    assert!(!palace_path.exists());

    // 4. Restore from the archive.
    let restore_args = RestoreArgs {
        archive: archive_path.clone(),
        palace: None,
        merge: false,
    };
    restore_to_root(&data_root, restore_args, &quiet_out()).expect("restore");

    // 5. Verify the palace is back and the triple survived.
    assert!(palace_path.exists(), "palace dir restored");
    assert!(palace_path.join("palace.json").exists());
    assert!(palace_path.join("kg.db").exists());
    assert_eq!(
        count_kg_triples(&palace_path).await,
        1,
        "KG triple should survive backup/restore round-trip"
    );
}

#[tokio::test]
async fn restore_into_existing_palace_errors_without_merge() {
    let workspace = tempdir().expect("workspace tmp");
    let data_root = workspace.path().join("data");
    std::fs::create_dir_all(&data_root).unwrap();

    let palace_id = PalaceId::new("conflict");
    let palace = Palace {
        id: palace_id,
        name: "conflict".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: data_root.join("conflict"),
    };
    let reg = PalaceRegistry::new();
    reg.create_palace(&data_root, palace).expect("create");

    // Backup it.
    let archive = workspace.path().join("conflict.tar.gz");
    backup_one(&data_root, "conflict", &archive, &quiet_out()).expect("backup");

    // Restore over the existing palace without --merge → must error.
    let args = RestoreArgs {
        archive: archive.clone(),
        palace: None,
        merge: false,
    };
    let err = restore_to_root(&data_root, args, &quiet_out()).unwrap_err();
    assert!(
        format!("{err:#}").contains("already exists"),
        "expected 'already exists' error, got: {err:#}"
    );

    // Restore again with --merge → must succeed.
    let args = RestoreArgs {
        archive,
        palace: None,
        merge: true,
    };
    restore_to_root(&data_root, args, &quiet_out()).expect("merge restore");
}

#[tokio::test]
async fn restore_with_palace_rename() {
    let workspace = tempdir().expect("workspace tmp");
    let data_root = workspace.path().join("data");
    std::fs::create_dir_all(&data_root).unwrap();

    let palace = Palace {
        id: PalaceId::new("orig"),
        name: "orig".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: data_root.join("orig"),
    };
    let reg = PalaceRegistry::new();
    reg.create_palace(&data_root, palace).expect("create");
    drop(reg);

    let archive = workspace.path().join("orig.tar.gz");
    backup_one(&data_root, "orig", &archive, &quiet_out()).expect("backup");

    // Wipe original.
    std::fs::remove_dir_all(data_root.join("orig")).unwrap();

    let args = RestoreArgs {
        archive,
        palace: Some("renamed".to_string()),
        merge: false,
    };
    restore_to_root(&data_root, args, &quiet_out()).expect("restore");
    assert!(data_root.join("renamed/palace.json").exists());
    let raw = std::fs::read_to_string(data_root.join("renamed/palace.json")).unwrap();
    assert!(
        raw.contains("\"renamed\""),
        "palace.json should be rewritten"
    );
}
