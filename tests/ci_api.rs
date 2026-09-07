use aruvici::{ci_api::dispatch, ci_store::Store, profiles::PlatformConfig};
use serde_json::{json, Value};

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
