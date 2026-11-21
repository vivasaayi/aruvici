use aruvici::{ci_api::dispatch, ci_store::Store, profiles::PlatformConfig};
use serde_json::{json, Value};
use base64::{engine::general_purpose::STANDARD, Engine as _};

fn config() -> (tempfile::TempDir, PlatformConfig, Store, i64) {
    let temp = aruvici::safety::tempdir().unwrap();
    let config = PlatformConfig {
        state_dir: temp.path().join("state"),
        targets: vec![],
    };
    config.validate().unwrap();
    let store = Store::open(&config.state_dir).unwrap();
    let id = store
        .enqueue(
            "desktop",
            "abc",
            "plan",
            "one",
            json!({"feature_id":"feature-1"}),
        )
        .unwrap();
    (temp, config, store, id)
}

#[test]
fn events_replay_and_run_details_preserve_context() {
    let (_temp, config, store, id) = config();
    store
        .event(id, Some("lint"), "log", json!({"text":"first"}))
        .unwrap();
    let initial = dispatch(&config, json!({"method":"events","id":id})).unwrap();
    let cursor = initial.as_array().unwrap().last().unwrap()["id"]
        .as_i64()
        .unwrap();
    store
        .event(id, Some("lint"), "log", json!({"text":"second"}))
        .unwrap();
    let replay = dispatch(&config, json!({"method":"events","id":id,"after":cursor})).unwrap();
    assert_eq!(replay.as_array().unwrap().len(), 1);
    assert_eq!(replay[0]["payload"]["text"], "second");
    assert_eq!(
        dispatch(&config, json!({"method":"events","id":id,"after":cursor})).unwrap(),
        replay
    );
    let run = dispatch(&config, json!({"method":"run","id":id})).unwrap();
    assert_eq!(run["context"]["feature_id"], "feature-1");
    assert_eq!(run["status"], "queued");
    assert!(dispatch(&config, json!({"method":"run","id":99999})).is_err());
}

#[test]
fn binary_artifact_chunks_are_bounded_and_lossless() {
    let (_temp, config, store, run) = config();
    let folder = config.state_dir.join("ci-runs").join(run.to_string());
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("bundle.bin");
    let binary: Vec<u8> = (0..70001).map(|n| (n % 256) as u8).collect();
    std::fs::write(&path, &binary).unwrap();
    let id = store.artifact(run, "package", "package", &path).unwrap();
    let metadata = dispatch(&config, json!({"method":"artifact","id":id})).unwrap();
    assert_eq!(metadata["size"], binary.len());
    let first = dispatch(
        &config,
        json!({"method":"artifact_chunk","id":id,"limit":9999999}),
    )
    .unwrap();
    assert_eq!(first["bytes"].as_array().unwrap().len(), 65536);
    assert_eq!(first["eof"], false);
    assert_eq!(first["sha256"], metadata["sha256"]);
    let second = dispatch(
        &config,
        json!({"method":"artifact_chunk","id":id,"offset":first["next_offset"]}),
    )
    .unwrap();
    assert_eq!(second["eof"], true);
    let received: Vec<u8> = first["bytes"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["bytes"].as_array().unwrap())
        .map(|v| v.as_u64().unwrap() as u8)
        .collect();
    assert_eq!(received, binary);
    let end = dispatch(
        &config,
        json!({"method":"artifact_chunk","id":id,"offset":binary.len()}),
    )
    .unwrap();
    assert_eq!(end["bytes"], json!([]));
    assert_eq!(end["eof"], true);
}

#[test]
fn base64_artifact_chunks_are_bounded_and_lossless() {
    let (_temp, config, store, run) = config();
    let folder = config.state_dir.join("ci-runs").join(run.to_string());
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("large.bin");
    let original: Vec<u8> = (0..524301).map(|i| (i % 256) as u8).collect();
    std::fs::write(&path, &original).unwrap();
    let id = store.artifact(run, "package", "package", &path).unwrap();
    let first = dispatch(&config, json!({"method":"artifact_chunk_base64","id":id})).unwrap();
    let mut result = STANDARD.decode(first["data_base64"].as_str().unwrap()).unwrap();
    assert_eq!(result.len(), 524288);
    assert_eq!(first["eof"], false);
    let second = dispatch(&config, json!({"method":"artifact_chunk_base64","id":id,"offset":first["next_offset"]})).unwrap();
    result.extend(STANDARD.decode(second["data_base64"].as_str().unwrap()).unwrap());
    assert_eq!(second["eof"], true);
    assert_eq!(result, original);
}

#[test]
fn arbitrary_paths_and_privileged_actions_are_unavailable() {
    let (temp, config, store, run) = config();
    let outside = temp.path().join("private.txt");
    std::fs::write(&outside, b"private").unwrap();
    assert!(dispatch(&config, json!({"method":"artifact_chunk","path":outside})).is_err());
    assert!(dispatch(
        &config,
        json!({"method":"artifact_chunk","id":9999,"path":outside})
    )
    .is_err());
    let state_file = config.state_dir.join("not-an-artifact.txt");
    std::fs::write(&state_file, b"state private").unwrap();
    let id = store.artifact(run, "test", "log", &state_file).unwrap();
    assert!(dispatch(&config, json!({"method":"artifact_chunk","id":id})).is_err());
    for method in ["approve", "deploy", "rollback", "execute", "shell"] {
        assert!(
            dispatch(&config, json!({"method":method,"id":run})).is_err(),
            "{method}"
        );
    }
    for stage in [
        "../private",
        "/etc/passwd",
        "nested/log",
        "..",
        "lint\\outside",
    ] {
        assert!(
            dispatch(&config, json!({"method":"logs","id":run,"stage":stage})).is_err(),
            "{stage}"
        );
    }
    let folder = config.state_dir.join("ci-runs").join(run.to_string());
    std::fs::create_dir_all(&folder).unwrap();
    std::os::unix::fs::symlink(&outside, folder.join("lint.log")).unwrap();
    assert!(dispatch(&config, json!({"method":"logs","id":run,"stage":"lint"})).is_err());
}

#[test]
fn artifact_chunks_cannot_read_another_runs_storage() {
    let (_temp, config, store, run) = config();
    let other_run = store.enqueue("desktop", "def", "plan", "two", json!({})).unwrap();
    let folder = config.state_dir.join("ci-runs").join(other_run.to_string());
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("other.bin");
    std::fs::write(&path, b"other run").unwrap();
    let mismatched_id = store.artifact(run, "package", "package", &path).unwrap();
    assert!(dispatch(&config, json!({"method":"artifact_chunk","id":mismatched_id})).is_err());
    assert!(dispatch(&config, json!({"method":"artifact_chunk_base64","id":mismatched_id})).is_err());
    assert!(dispatch(&config, json!({"method":"promotion_readiness","id":mismatched_id})).is_err());
}

#[test]
fn logs_are_bounded_and_cancellation_is_durable() {
    let (_temp, config, store, run) = config();
    let folder = config.state_dir.join("ci-runs").join(run.to_string());
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("lint.log"), vec![b'a'; 70000]).unwrap();
    let first = dispatch(&config, json!({"method":"logs","id":run,"stage":"lint"})).unwrap();
    assert_eq!(first["text"].as_str().unwrap().len(), 65536);
    let second = dispatch(
        &config,
        json!({"method":"logs","id":run,"stage":"lint","offset":first["next_offset"]}),
    )
    .unwrap();
    assert_eq!(second["text"].as_str().unwrap().len(), 4464);
    let cancelled = dispatch(&config, json!({"method":"cancel","id":run})).unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    assert!(store.cancelled(run).unwrap());
    assert_eq!(
        dispatch(&config, json!({"method":"health"})).unwrap()["protocol"],
        1
    );
    assert_eq!(
        dispatch(&config, json!({"method":"targets"})).unwrap(),
        Value::Array(vec![])
    );
}

#[test]
fn audit_artifact_replaced_with_symlink() {
    let (temp, config, store, run) = config();
    let folder = config.state_dir.join("ci-runs").join(run.to_string());
    std::fs::create_dir_all(&folder).unwrap();
    let artifact = folder.join("package.bin");
    let private = temp.path().join("private.txt");
    std::fs::write(&artifact, b"original").unwrap();
    std::fs::write(&private, b"secret!!").unwrap();
    let id = store
        .artifact(run, "package", "package", &artifact)
        .unwrap();
    std::fs::remove_file(&artifact).unwrap();
    std::os::unix::fs::symlink(&private, &artifact).unwrap();
    let error = dispatch(&config, json!({"method":"artifact_chunk", "id":id})).unwrap_err();
    assert!(error.to_string().contains("symlink not permitted"));
}

#[test]
fn targets_report_repository_and_current_approval() {
    use aruvici::{ci, profiles::Target};
    use std::collections::BTreeMap;
    let (_temp, mut config, _store, _) = config();
    let repository = config.state_dir.parent().unwrap().join("repository");
    std::fs::create_dir(&repository).unwrap();
    config.targets.push(Target {
        id: "fixture".into(),
        repository: repository.clone(),
        root: ".".into(),
        profile: "native-ios@1".into(),
        inputs: BTreeMap::new(),
        preview: None,
    });
    let list = || dispatch(&config, json!({"method":"targets"})).unwrap();
    let before = list();
    assert_eq!(before[0]["repository"], repository.to_str().unwrap());
    assert_eq!(before[0]["approval"]["approved"], false);
    let plan = config.targets[0].plan().unwrap();
    ci::approve(&config, "fixture", &plan.digest).unwrap();
    assert_eq!(list()[0]["approval"]["approved"], true);
    std::fs::remove_file(config.state_dir.join("ci-approvals/fixture.json")).unwrap();
    assert_eq!(list()[0]["approval"]["approved"], false);
}

#[test]
fn retry_preserves_provenance_and_requires_current_approval() {
    use aruvici::{ci, profiles::Target};
    use std::{collections::BTreeMap, process::Command};
    let (_temp, mut config, store, _) = config();
    let repository = config.state_dir.parent().unwrap().join("repository");
    std::fs::create_dir(&repository).unwrap();
    std::fs::write(
        repository.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'\n",
    )
    .unwrap();
    std::fs::write(repository.join("Dockerfile"), "FROM scratch\n").unwrap();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .current_dir(&repository)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-qm",
        "fixture",
    ]);
    let commit = git(&["rev-parse", "HEAD"]);
    config.targets.push(Target {
        id: "fixture".into(),
        repository,
        root: ".".into(),
        profile: "rust-docker@1".into(),
        inputs: BTreeMap::new(),
        preview: None,
    });
    let plan = config.targets[0].plan().unwrap();
    assert!(plan.issues.is_empty(), "{:?}", plan.issues);
    let original = store
        .enqueue(
            "fixture",
            &commit,
            &plan.digest,
            "original",
            json!({"product_id":"fixture"}),
        )
        .unwrap();
    assert!(dispatch(
        &config,
        json!({"method":"retry","id":original,"key":"retry-key"})
    )
    .is_err());
    let _ = store.claim().unwrap(); // The unrelated fixture run comes first.
    store.finish(1, "failed", None).unwrap();
    assert_eq!(store.claim().unwrap().unwrap()["id"], original);
    store.finish(original, "failed", None).unwrap();
    assert!(dispatch(
        &config,
        json!({"method":"retry","id":original,"key":"retry-key"})
    )
    .is_err());
    ci::approve(&config, "fixture", &plan.digest).unwrap();
    let result = dispatch(
        &config,
        json!({"method":"retry","id":original,"key":"retry-key"}),
    )
    .unwrap();
    let retried = result["run_id"].as_i64().unwrap();
    assert_ne!(retried, original);
    assert_eq!(store.run(retried).unwrap()["commit"], commit);
    assert_eq!(
        store.run(retried).unwrap()["context"],
        json!({"product_id":"fixture"})
    );
    assert_eq!(
        dispatch(
            &config,
            json!({"method":"retry","id":original,"key":"retry-key"})
        )
        .unwrap()["run_id"],
        retried
    );
    config.targets[0]
        .inputs
        .insert("image".into(), "changed-image".into());
    assert!(dispatch(
        &config,
        json!({"method":"retry","id":original,"key":"another-key"})
    )
    .is_err());
}

#[test]
fn artifact_readiness_verifies_run_ownership_and_content_without_authorizing_promotion() {
    let (temp, config, store, run) = config();
    let root = config.state_dir.join("ci-runs").join(run.to_string());
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("package.bin");
    std::fs::write(&path, b"evidence").unwrap();
    let id = store.artifact(run, "package", "package", &path).unwrap();
    let result = dispatch(&config, json!({"method":"promotion_readiness","id":id})).unwrap();
    assert_eq!(result["run_id"], run);
    assert_eq!(result["artifact_id"], id);
    assert_eq!(result["eligible"], false);
    assert_eq!(result["manual_promotion_only"], true);
    std::fs::write(&path, b"tampered").unwrap();
    assert!(dispatch(&config, json!({"method":"promotion_readiness","id":id})).is_err());
    std::fs::remove_file(&path).unwrap();
    let outside = temp.path().join("private");
    std::fs::write(&outside, b"evidence").unwrap();
    std::os::unix::fs::symlink(&outside, &path).unwrap();
    assert!(dispatch(&config, json!({"method":"promotion_readiness","id":id})).is_err());
    let wrong_root = config.state_dir.join("ci-runs").join("999");
    std::fs::create_dir_all(&wrong_root).unwrap();
    let wrong_path = wrong_root.join("package.bin");
    std::fs::write(&wrong_path, b"evidence").unwrap();
    let wrong_id = store
        .artifact(run, "package", "package", &wrong_path)
        .unwrap();
    assert!(dispatch(
        &config,
        json!({"method":"promotion_readiness","id":wrong_id})
    )
    .is_err());
}

#[test]
fn artifact_reads_reject_hard_links_to_other_files() {
    let (temp, config, store, run) = config();
    let root = config.state_dir.join("ci-runs").join(run.to_string());
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("package.bin");
    std::fs::write(&path, b"fixture").unwrap();
    let id = store.artifact(run, "package", "package", &path).unwrap();
    let other = temp.path().join("outside-hard-link");
    std::fs::hard_link(&path, other).unwrap();
    for method in ["artifact_chunk", "artifact_chunk_base64", "promotion_readiness"] {
        let result = dispatch(&config, json!({"method": method, "id": id}));
        assert!(result.is_err(), "{method} accepted a hard-linked artifact");
    }
}
