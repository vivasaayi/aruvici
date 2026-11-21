use aruvici::{ci_api, safety};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::json;
use std::{
    fs,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn foreground_service_serves_private_socket_and_stops_cleanly() {
    let temp = tempfile::tempdir_in("/private/tmp").unwrap();
    let state = temp.path().join("state");
    let config = temp.path().join("platform.toml");
    fs::write(
        &config,
        format!("state_dir = {:?}\ntargets = []\n", state.to_str().unwrap()),
    )
    .unwrap();
    let socket = state.join("ci.sock");
    let child = Command::new(env!("CARGO_BIN_EXE_aruvici"))
        .args(["ci", "--platform"])
        .arg(&config)
        .arg("serve")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    struct Guard(std::process::Child);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Guard(child);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if socket.exists() {
            break;
        }
        assert!(Instant::now() < deadline, "service failed to bind");
        assert!(child.0.try_wait().unwrap().is_none(), "service exited");
        std::thread::sleep(Duration::from_millis(30));
    }
    safety::no_symlinks(&socket).unwrap();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        ci_api::request(&socket, json!({"method":"health"})).unwrap()["result"]["protocol"],
        1
    );
    assert_eq!(
        ci_api::request(&socket, json!({"method":"runs"})).unwrap()["result"],
        json!([])
    );
    assert_eq!(
        ci_api::request(&socket, json!({"method":"deploy"})).unwrap()["ok"],
        false
    );
    // Exercise a response much larger than the socket buffer through the real
    // server, not only dispatch(), to catch partial or truncated writes.
    let store = aruvici::ci_store::Store::open(&state).unwrap();
    let run = store.enqueue("fixture", "abc", "plan", "socket-artifact", json!({})).unwrap();
    let directory = state.join("ci-runs").join(run.to_string());
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("socket.bin");
    let expected: Vec<u8> = (0..524288).map(|i| (i % 256) as u8).collect();
    fs::write(&path, &expected).unwrap();
    let artifact = store.artifact(run, "package", "package", &path).unwrap();
    let response = ci_api::request(&socket, json!({"method":"artifact_chunk_base64", "id":artifact})).unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["result"]["next_offset"], 524288);
    assert_eq!(STANDARD.decode(response["result"]["data_base64"].as_str().unwrap()).unwrap(), expected);
    // SAFETY: signal only the child PID owned by this test.
    unsafe {
        libc::kill(child.0.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "service ignored termination");
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(!socket.exists());
}
