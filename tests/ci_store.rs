use aruvici::ci_store::Store;
use serde_json::json;

#[test]
fn enqueue_is_idempotent_and_claim_is_exclusive() {
    let temp = aruvici::safety::tempdir().unwrap();
    let a = Store::open(temp.path()).unwrap();
    let b = Store::open(temp.path()).unwrap();
    let id = a
        .enqueue("desktop", "abc", "plan", "manual-1", json!({"feature":"f"}))
        .unwrap();
    assert_eq!(
        id,
        b.enqueue("desktop", "abc", "plan", "manual-1", json!({}))
            .unwrap()
    );
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let other = barrier.clone();
    let first = std::thread::spawn(move || {
        barrier.wait();
        a.claim().unwrap()
    });
    let second = std::thread::spawn(move || {
        other.wait();
        b.claim().unwrap()
    });
    let claims = [first.join().unwrap(), second.join().unwrap()];
    assert_eq!(claims.iter().filter(|c| c.is_some()).count(), 1);
    let store = Store::open(temp.path()).unwrap();
    assert_eq!(store.events(id, 0).unwrap().as_array().unwrap().len(), 2);
    assert_ne!(
        id,
        store
            .enqueue("desktop", "abc", "plan", "retry-2", json!({}))
            .unwrap()
    );
    assert_eq!(store.run(id).unwrap()["context"]["feature"], "f");
}

#[test]
fn events_are_cursor_ordered_and_stages_survive_restart() {
    let temp = aruvici::safety::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let id = store
        .enqueue("desktop", "abc", "plan", "1", json!({}))
        .unwrap();
    store.claim().unwrap();
    store.stage(id, "lint", "running", json!({})).unwrap();
    store
        .event(id, Some("lint"), "started", json!({"line":1}))
        .unwrap();
    store
        .event(id, Some("lint"), "log", json!({"line":2}))
        .unwrap();
    let events = store.events(id, 0).unwrap();
    assert_eq!(events.as_array().unwrap().len(), 5);
    let cursor = events[3]["id"].as_i64().unwrap();
    assert_eq!(store.events(id, cursor).unwrap()[0]["payload"]["line"], 2);
    drop(store);
    let reopened = Store::open(temp.path()).unwrap();
    assert_eq!(reopened.run(id).unwrap()["status"], "running");
    reopened.recover_interrupted().unwrap();
    let run = reopened.run(id).unwrap();
    assert_eq!(run["status"], "interrupted");
    assert_eq!(run["stages"][0]["status"], "interrupted");
    assert!(reopened.claim().unwrap().is_none());
}

#[test]
fn cancellation_preserves_running_work_until_worker_acknowledges() {
    let temp = aruvici::safety::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let a = store.enqueue("a", "abc", "p", "1", json!({})).unwrap();
    let b = store.enqueue("b", "abc", "p", "1", json!({})).unwrap();
    store.cancel(a).unwrap();
    assert_eq!(store.run(a).unwrap()["status"], "cancelled");
    assert_eq!(store.claim().unwrap().unwrap()["id"], b);
    store.cancel(b).unwrap();
    assert!(store.cancelled(b).unwrap());
    assert_eq!(store.run(b).unwrap()["status"], "running");
    store.stage(b, "package", "pending", json!({})).unwrap();
    store.finish(b, "cancelled", None).unwrap();
    assert_eq!(store.run(b).unwrap()["stages"][0]["status"], "blocked");
    assert!(store.finish(b, "succeeded", None).is_err());
    assert!(store.cancel(999).is_err());
}

#[test]
fn artifacts_are_hashed_and_confined_to_state() {
    let temp = aruvici::safety::tempdir().unwrap();
    let state = temp.path().join("state");
    let store = Store::open(&state).unwrap();
    let run = store.enqueue("a", "abc", "p", "1", json!({})).unwrap();
    let file = state.join("result.txt");
    std::fs::write(&file, b"abc").unwrap();
    let id = store.artifact(run, "build", "log", &file).unwrap();
    let info = store.artifact_info(id).unwrap();
    assert_eq!(
        info["sha256"],
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(info["size"], 3);
    let outside = temp.path().join("outside.txt");
    std::fs::write(&outside, b"private").unwrap();
    assert!(store.artifact(run, "build", "log", &outside).is_err());
    let link = state.join("link");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    assert!(store.artifact(run, "build", "log", &link).is_err());
    assert!(store.artifact(run, "build", "log", &state).is_err());
}
