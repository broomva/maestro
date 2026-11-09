//! `tree::remote_ref` and its parser (BRO-3635): where a remote says a ref is, bounded in time and
//! output, taken only for exactly that ref, and only as a well-formed object id.

use maestro_store::tree::{MAX_LS_REMOTE, parse_ls_remote, remote_ref};
use std::time::{Duration, Instant};

const A: &str = "1111111111111111111111111111111111111111";
const B: &str = "2222222222222222222222222222222222222222";

#[test]
fn only_the_exact_ref_counts() {
    // ls-remote's patterns match by suffix, so it prints refs/heads/x/refs/heads/main too.
    let out = format!("{A}\trefs/heads/x/refs/heads/main\n{B}\trefs/heads/main\n");
    assert_eq!(parse_ls_remote(out.as_bytes(), "refs/heads/main").unwrap().as_deref(), Some(B));
    assert_eq!(parse_ls_remote(format!("{A}\tHEAD\n").as_bytes(), "refs/heads/main").unwrap(), None);
    let sha256 = "3".repeat(64);
    assert_eq!(
        parse_ls_remote(format!("{sha256}\trefs/heads/main\n").as_bytes(), "refs/heads/main").unwrap(),
        Some(sha256)
    );
}

#[test]
fn a_second_line_for_the_ref_or_a_malformed_id_is_refused() {
    let twice = format!("{A}\trefs/heads/main\n{B}\trefs/heads/main\n");
    assert!(parse_ls_remote(twice.as_bytes(), "refs/heads/main").is_err());
    for bad in ["123", &"g".repeat(40), &"A".repeat(40), &"1".repeat(41)] {
        let out = format!("{bad}\trefs/heads/main\n");
        assert!(parse_ls_remote(out.as_bytes(), "refs/heads/main").is_err(), "{bad}");
    }
}

#[test]
fn output_over_the_bound_is_refused() {
    let mut out = format!("{A}\trefs/heads/main\n").into_bytes();
    out.extend(vec![b'#'; MAX_LS_REMOTE]);
    assert!(parse_ls_remote(&out, "refs/heads/main").is_err());
}

#[test]
fn a_remote_that_hangs_is_given_up_at_the_timeout() {
    // A git:// server that accepts and never says a word.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let held = listener.accept();
        std::thread::sleep(Duration::from_secs(30));
        drop(held);
    });
    let (tx, rx) = std::sync::mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        let _ = tx.send(remote_ref(&format!("git://127.0.0.1:{port}/x"), "refs/heads/main", Duration::from_secs(1)));
    });
    let got = rx.recv_timeout(Duration::from_secs(15)).expect("remote_ref never gave up");
    let e = got.unwrap_err();
    assert!(e.contains("did not answer"), "{e}");
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}
