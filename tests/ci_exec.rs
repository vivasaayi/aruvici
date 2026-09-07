use aruvici::{ci_exec::execute, process::args, safety::tempdir};
use std::{
    fs,
    time::{Duration, Instant},
};

#[test]
fn captures_success_failure_and_redacts_both_streams() {
    let dir = tempdir().unwrap();
    let log = dir.path().join("step.log");
    let result = execute(&args(&["/bin/sh", "-c", "printf 'ordinary output\\n'; printf '%s\\n' \"$TEST_TOKEN\"; printf 'Authorization: Bearer another\\n' >&2; exit 7"]), dir.path(), &[("TEST_TOKEN".into(), "very-private-value".into())], &log, Duration::from_secs(5), || false).unwrap();
    assert_eq!(result.status, "failed");
    assert_eq!(result.exit_code, Some(7));
    let text = fs::read_to_string(log).unwrap();
    assert!(text.contains("ordinary output"));
    assert!(!text.contains("very-private-value"));
    assert!(!text.contains("another"));
}

#[test]
fn missing_tool_is_an_error() {
    let dir = tempdir().unwrap();
    assert!(execute(
        &args(&["/definitely-missing-aruvici-command"]),
        dir.path(),
        &[],
        &dir.path().join("log"),
        Duration::from_secs(1),
        || false
    )
    .is_err());
}

#[test]
fn timeout_and_cancellation_stop_process_groups() {
    for cancelled in [false, true] {
        let dir = tempdir().unwrap();
        let started = Instant::now();
        let result = execute(
            &args(&["/bin/sh", "-c", "sleep 30 & wait"]),
            dir.path(),
            &[],
            &dir.path().join("log"),
            Duration::from_millis(200),
            || cancelled && started.elapsed() >= Duration::from_millis(50),
        )
        .unwrap();
        assert_eq!(
            result.status,
            if cancelled { "cancelled" } else { "timed_out" }
        );
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}

#[test]
fn oversized_output_is_suppressed_and_disk_is_bounded() {
    let dir = tempdir().unwrap();
    let log = dir.path().join("log");
    let result = execute(
        &args(&["/bin/sh", "-c", "head -c 11000000 /dev/zero; printf '\\n'"]),
        dir.path(),
        &[],
        &log,
        Duration::from_secs(10),
        || false,
    )
    .unwrap();
    assert_eq!(result.status, "passed");
    assert!(result.truncated);
    assert!(fs::metadata(log).unwrap().len() <= aruvici::ci_exec::LOG_LIMIT as u64);
}

#[test]
fn many_short_lines_are_drained_after_log_limit() {
    let dir = tempdir().unwrap();
    let log = dir.path().join("log");
    let result = execute(
        &args(&[
            "/usr/bin/awk",
            "BEGIN {for(i=0;i<11000;i++) printf \"%01024d\\n\", i;}",
        ]),
        dir.path(),
        &[],
        &log,
        Duration::from_secs(15),
        || false,
    )
    .unwrap();
    assert_eq!(result.status, "passed");
    assert!(result.truncated);
    let size = fs::metadata(log).unwrap().len();
    assert!(size <= aruvici::ci_exec::LOG_LIMIT as u64);
    assert!(size > 9 * 1024 * 1024);
}

#[test]
fn private_key_body_is_never_persisted() {
    let dir = tempdir().unwrap();
    let log = dir.path().join("log");
    execute(&args(&["/bin/sh", "-c", "printf '%s\\n' '-----BEGIN PRIVATE KEY-----' 'private-body-value' '-----END PRIVATE KEY-----'"]), dir.path(), &[], &log, Duration::from_secs(5), || false).unwrap();
    assert!(!fs::read_to_string(log)
        .unwrap()
        .contains("private-body-value"));
}

#[test]
fn refuses_symlink_or_existing_log() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("existing");
    fs::write(&target, "preserve me").unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    for log in [target.clone(), link] {
        assert!(execute(
            &args(&["/usr/bin/true"]),
            dir.path(),
            &[],
            &log,
            Duration::from_secs(1),
            || false
        )
        .is_err());
    }
    assert_eq!(fs::read_to_string(target).unwrap(), "preserve me");
}
