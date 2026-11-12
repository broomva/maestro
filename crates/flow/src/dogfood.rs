//! The verifier pilot's dogfood receipt, `dogfood-receipt@1`, and its check (BRO-3636): the verify
//! the kernel runs for `verify@1`'s `exercise` node (contract V4: "its verify validates a dogfood
//! receipt bound to `pin.head`"), as `maestro dogfood verify-receipt`.
//!
//! # Where the receipt is
//!
//! The performer writes the receipt where it likes and completes with outputs that name it and
//! its digest: `{"receipt": "<path>", "receipt_sha256": "sha256:<hex>"}` (other fields are let
//! be). The kernel hands a work node's verify the outputs it judges as JSON on its standard input,
//! never in its argv, so no text of the performer's is read as an argument or as code; this check
//! reads them there ([`verify`] takes the bytes). A relative path is taken from the flow's
//! workdir, where the kernel runs the verify. The kernel records the outputs in `Completed`, so the
//! log pins the receipt's content by its digest, and the receipt pins its evidence by its own: a
//! receipt or evidence changed after the completion no longer matches what the log holds. The
//! receipt stays outside the flow ledger, which this module never opens.
//!
//! # The receipt
//!
//! One JSON object, at most [`MAX_RECEIPT`] bytes, with exactly these fields:
//!
//! | Field | What it holds |
//! |---|---|
//! | `schema` | `"dogfood-receipt@1"` |
//! | `head` | the commit the surface was driven at, as `params.head` names it |
//! | `surface` | the surface driven, as `params.surface` names it |
//! | `entry` | what a user touches first: a URL, a command, a window |
//! | `driver` | what drove it: curl, a browser driver, a shell |
//! | `evidence` | `{"path", "sha256"}`: the file that shows it (a log, a screenshot, a response body), absolute or relative to the receipt's folder, at most [`MAX_EVIDENCE`] bytes, and its `sha256:<64 hex>` |
//! | `result` | `"pass"` or `"fail"` |
//! | `at` | when it was driven, an RFC 3339 timestamp |
//!
//! # The check
//!
//! [`verify`] reads nothing but its input, the receipt and its evidence, and writes nothing. Each
//! file is opened without following a symbolic link at its last component and without blocking
//! (`O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`), then checked to be a regular file before a byte is
//! read, so a link, a FIFO or a device fails at once and never hangs the check. It accepts a
//! receipt only when the input names it and its digest, it hashes to that digest, it is well
//! formed, names the head and surface it is asked about and says pass, and its evidence hashes to
//! the digest it records. It fails closed at the first check that does not hold, in this order,
//! each with its own [`Failure`] and exit status:
//!
//! | Exit | Code | |
//! |---|---|---|
//! | 11 | `outputs_invalid` | the input is not a JSON object with `receipt` and a `sha256:<64 hex>` `receipt_sha256`, or is over [`MAX_OUTPUTS`] |
//! | 3 | `receipt_missing` | no file at the path |
//! | 13 | `receipt_not_regular` | a symbolic link, a FIFO, a folder, a device |
//! | 4 | `receipt_unreadable` | it cannot be read, or is over [`MAX_RECEIPT`] |
//! | 12 | `receipt_changed` | it does not hash to `receipt_sha256` |
//! | 5 | `receipt_malformed` | not a `dogfood-receipt@1` |
//! | 6 | `wrong_head` | for another head |
//! | 7 | `wrong_surface` | for another surface |
//! | 8 | `result_not_pass` | its result is fail |
//! | 14 | `evidence_not_regular` | its evidence is a symbolic link, a FIFO, a folder, a device |
//! | 9 | `evidence_unreadable` | missing, unreadable, or over [`MAX_EVIDENCE`] |
//! | 10 | `evidence_mismatch` | it does not hash to the receipt's digest |
//!
//! Exit 2 stays clap's, for arguments that do not parse.

use rustix::fs::{FileType, Mode, OFlags};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

/// The schema, and the verifier's name in a receipt's records.
pub const SCHEMA: &str = "dogfood-receipt@1";

/// The most bytes of input (the outputs JSON) the check reads: the kernel's own bound on outputs.
pub const MAX_OUTPUTS: usize = crate::kernel::MAX_OUTPUTS;

/// The most bytes a receipt may have.
pub const MAX_RECEIPT: usize = 64 << 10;

/// The most bytes evidence may have: a screenshot or a recording, not an archive.
pub const MAX_EVIDENCE: u64 = 1 << 30;

/// The longest `head`, `surface`, `entry`, `driver` or evidence path a receipt may hold.
pub const MAX_FIELD: usize = 4096;

/// A dogfood receipt, as written. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema: String,
    pub head: String,
    pub surface: String,
    pub entry: String,
    pub driver: String,
    pub evidence: Evidence,
    pub result: Outcome,
    pub at: String,
}

/// The file that shows the surface was driven, and its digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub path: String,
    pub sha256: String,
}

/// What driving the surface came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Pass,
    Fail,
}

/// What the check reads of its input, the outputs the performer completed with.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Named {
    pub receipt: String,
    pub receipt_sha256: String,
}

/// An accepted receipt: what it was checked against, and the digests of what was read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Accepted {
    pub verifier: &'static str,
    pub receipt: PathBuf,
    pub receipt_sha256: String,
    pub head: String,
    pub surface: String,
    pub evidence: PathBuf,
    pub evidence_sha256: String,
}

/// Why a receipt is not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    OutputsInvalid { detail: String },
    Missing { path: PathBuf },
    NotRegular { path: PathBuf, detail: String },
    Unreadable { path: PathBuf, detail: String },
    Changed { want: String, got: String },
    Malformed { detail: String },
    WrongHead { want: String, got: String },
    WrongSurface { want: String, got: String },
    NotPassed,
    EvidenceNotRegular { path: PathBuf, detail: String },
    EvidenceUnreadable { path: PathBuf, detail: String },
    EvidenceMismatch { path: PathBuf, want: String, got: String },
}

impl Failure {
    /// A stable code for the failure.
    pub fn code(&self) -> &'static str {
        match self {
            Failure::OutputsInvalid { .. } => "outputs_invalid",
            Failure::Missing { .. } => "receipt_missing",
            Failure::NotRegular { .. } => "receipt_not_regular",
            Failure::Unreadable { .. } => "receipt_unreadable",
            Failure::Changed { .. } => "receipt_changed",
            Failure::Malformed { .. } => "receipt_malformed",
            Failure::WrongHead { .. } => "wrong_head",
            Failure::WrongSurface { .. } => "wrong_surface",
            Failure::NotPassed => "result_not_pass",
            Failure::EvidenceNotRegular { .. } => "evidence_not_regular",
            Failure::EvidenceUnreadable { .. } => "evidence_unreadable",
            Failure::EvidenceMismatch { .. } => "evidence_mismatch",
        }
    }

    /// The exit status `maestro dogfood verify-receipt` gives for it, one per failure.
    pub fn exit(&self) -> u8 {
        match self {
            Failure::Missing { .. } => 3,
            Failure::Unreadable { .. } => 4,
            Failure::Malformed { .. } => 5,
            Failure::WrongHead { .. } => 6,
            Failure::WrongSurface { .. } => 7,
            Failure::NotPassed => 8,
            Failure::EvidenceUnreadable { .. } => 9,
            Failure::EvidenceMismatch { .. } => 10,
            Failure::OutputsInvalid { .. } => 11,
            Failure::Changed { .. } => 12,
            Failure::NotRegular { .. } => 13,
            Failure::EvidenceNotRegular { .. } => 14,
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::OutputsInvalid { detail } => write!(f, "the outputs do not name a receipt: {detail}"),
            Failure::Missing { path } => write!(f, "no receipt at {}", path.display()),
            Failure::NotRegular { path, detail } => {
                write!(f, "the receipt at {} is not a regular file: {detail}", path.display())
            }
            Failure::Unreadable { path, detail } => {
                write!(f, "the receipt at {} cannot be read: {detail}", path.display())
            }
            Failure::Changed { want, got } => write!(f, "the receipt hashes to {got}, not the outputs' {want}"),
            Failure::Malformed { detail } => write!(f, "the receipt is not a {SCHEMA}: {detail}"),
            Failure::WrongHead { want, got } => write!(f, "the receipt is for head {got}, not {want}"),
            Failure::WrongSurface { want, got } => write!(f, "the receipt is for surface {got}, not {want}"),
            Failure::NotPassed => write!(f, "the receipt's result is fail"),
            Failure::EvidenceNotRegular { path, detail } => {
                write!(f, "the evidence at {} is not a regular file: {detail}", path.display())
            }
            Failure::EvidenceUnreadable { path, detail } => {
                write!(f, "the evidence at {} cannot be read: {detail}", path.display())
            }
            Failure::EvidenceMismatch { path, want, got } => {
                write!(f, "the evidence at {} hashes to {got}, not the receipt's {want}", path.display())
            }
        }
    }
}

impl std::error::Error for Failure {}

/// Checks the receipt `input` (the outputs JSON) names against `head` and `surface`. See the
/// module docs.
pub fn verify(input: &[u8], head: &str, surface: &str) -> Result<Accepted, Failure> {
    let named = outputs(input)?;
    let path = PathBuf::from(&named.receipt);
    let bytes = read_receipt(&path)?;
    let receipt_sha256 = sha256(&bytes);
    if receipt_sha256 != named.receipt_sha256 {
        return Err(Failure::Changed { want: named.receipt_sha256, got: receipt_sha256 });
    }
    let receipt = parse(&bytes)?;
    if receipt.head != head {
        return Err(Failure::WrongHead { want: head.to_string(), got: receipt.head });
    }
    if receipt.surface != surface {
        return Err(Failure::WrongSurface { want: surface.to_string(), got: receipt.surface });
    }
    if receipt.result != Outcome::Pass {
        return Err(Failure::NotPassed);
    }
    let evidence = evidence_path(&path, &receipt.evidence.path);
    let got = hash_evidence(&evidence)?;
    if got != receipt.evidence.sha256 {
        return Err(Failure::EvidenceMismatch { path: evidence, want: receipt.evidence.sha256, got });
    }
    Ok(Accepted {
        verifier: SCHEMA,
        receipt: path,
        receipt_sha256,
        head: receipt.head,
        surface: receipt.surface,
        evidence,
        evidence_sha256: got,
    })
}

/// The receipt and digest `input` names: a JSON object of at most [`MAX_OUTPUTS`] bytes with a
/// non-empty `receipt` and a `sha256:<64 hex>` `receipt_sha256`.
pub fn outputs(input: &[u8]) -> Result<Named, Failure> {
    let invalid = |detail: String| Failure::OutputsInvalid { detail };
    if input.len() > MAX_OUTPUTS {
        return Err(invalid(format!("they are more than {MAX_OUTPUTS} bytes")));
    }
    let named: Named = serde_json::from_slice(input).map_err(|e| invalid(e.to_string()))?;
    if named.receipt.is_empty() || named.receipt.len() > MAX_FIELD {
        return Err(invalid(format!("receipt is empty or more than {MAX_FIELD} bytes")));
    }
    if !is_digest(&named.receipt_sha256) {
        return Err(invalid("receipt_sha256 is not sha256:<64 lower-case hex>".to_string()));
    }
    Ok(named)
}

/// Why a file was not opened.
enum Unopened {
    Missing,
    NotRegular(String),
    Unreadable(String),
}

/// `path` opened to read: never through a symbolic link at its last component and never
/// blocking, then checked to be a regular file before anything is read.
fn open_regular(path: &Path) -> Result<std::fs::File, Unopened> {
    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = match rustix::fs::open(path, flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Err(Unopened::Missing),
        Err(Errno::LOOP) => return Err(Unopened::NotRegular("it is a symbolic link".to_string())),
        Err(e) => return Err(Unopened::Unreadable(e.to_string())),
    };
    let stat = rustix::fs::fstat(&fd).map_err(|e| Unopened::Unreadable(e.to_string()))?;
    let kind = FileType::from_raw_mode(stat.st_mode);
    if kind != FileType::RegularFile {
        return Err(Unopened::NotRegular(format!("it is a {kind:?}")));
    }
    Ok(std::fs::File::from(fd))
}

/// The receipt's bytes: a regular file of at most [`MAX_RECEIPT`] bytes, read to one byte past it.
fn read_receipt(path: &Path) -> Result<Vec<u8>, Failure> {
    let unreadable = |detail: String| Failure::Unreadable { path: path.to_path_buf(), detail };
    let file = open_regular(path).map_err(|e| match e {
        Unopened::Missing => Failure::Missing { path: path.to_path_buf() },
        Unopened::NotRegular(detail) => Failure::NotRegular { path: path.to_path_buf(), detail },
        Unopened::Unreadable(detail) => unreadable(detail),
    })?;
    let mut bytes = Vec::new();
    file.take(MAX_RECEIPT as u64 + 1).read_to_end(&mut bytes).map_err(|e| unreadable(e.to_string()))?;
    if bytes.len() > MAX_RECEIPT {
        return Err(unreadable(format!("it is more than {MAX_RECEIPT} bytes")));
    }
    Ok(bytes)
}

/// The receipt, well formed: the schema, every field, none empty or over [`MAX_FIELD`], a
/// `sha256:<64 hex>` digest and an RFC 3339 `at`.
pub fn parse(bytes: &[u8]) -> Result<Receipt, Failure> {
    let malformed = |detail: String| Failure::Malformed { detail };
    let receipt: Receipt = serde_json::from_slice(bytes).map_err(|e| malformed(e.to_string()))?;
    if receipt.schema != SCHEMA {
        return Err(malformed(format!("its schema is {:?}", receipt.schema)));
    }
    for (name, value) in [
        ("head", &receipt.head),
        ("surface", &receipt.surface),
        ("entry", &receipt.entry),
        ("driver", &receipt.driver),
        ("evidence.path", &receipt.evidence.path),
    ] {
        if value.trim().is_empty() {
            return Err(malformed(format!("{name} is empty")));
        }
        if value.len() > MAX_FIELD {
            return Err(malformed(format!("{name} is more than {MAX_FIELD} bytes")));
        }
    }
    if !is_digest(&receipt.evidence.sha256) {
        return Err(malformed("evidence.sha256 is not sha256:<64 lower-case hex>".to_string()));
    }
    if receipt.at.parse::<jiff::Timestamp>().is_err() {
        return Err(malformed(format!("at is not an RFC 3339 timestamp: {:?}", receipt.at)));
    }
    Ok(receipt)
}

fn is_digest(text: &str) -> bool {
    text.strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// The evidence's path: as written when absolute, from the receipt's folder otherwise.
fn evidence_path(receipt: &Path, evidence: &str) -> PathBuf {
    let evidence = Path::new(evidence);
    match receipt.parent() {
        Some(dir) if evidence.is_relative() => dir.join(evidence),
        _ => evidence.to_path_buf(),
    }
}

/// `sha256:<hex>` of the evidence, a regular file of at most [`MAX_EVIDENCE`] bytes.
fn hash_evidence(path: &Path) -> Result<String, Failure> {
    hash_bounded(path, MAX_EVIDENCE)
}

/// `sha256:<hex>` of a regular file of at most `limit` bytes, read in chunks to one byte past it.
fn hash_bounded(path: &Path, limit: u64) -> Result<String, Failure> {
    let unreadable = |detail: String| Failure::EvidenceUnreadable { path: path.to_path_buf(), detail };
    let file = open_regular(path).map_err(|e| match e {
        Unopened::Missing => unreadable("there is no file there".to_string()),
        Unopened::NotRegular(detail) => Failure::EvidenceNotRegular { path: path.to_path_buf(), detail },
        Unopened::Unreadable(detail) => unreadable(detail),
    })?;
    let mut file = file.take(limit + 1);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 << 10];
    let mut read: u64 = 0;
    loop {
        let n = file.read(&mut buf).map_err(|e| unreadable(e.to_string()))?;
        if n == 0 {
            break;
        }
        read += n as u64;
        hasher.update(&buf[..n]);
    }
    if read > limit {
        return Err(unreadable(format!("it is more than {limit} bytes")));
    }
    Ok(format!("sha256:{}", hex(&hasher.finalize())))
}

/// `sha256:<hex>` of `bytes`.
pub fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{}", hex(&Sha256::digest(bytes)))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const HEAD: &str = "9f2c3b1a9f2c3b1a9f2c3b1a9f2c3b1a9f2c3b1a";

    #[test]
    fn a_file_is_opened_closed_on_exec() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f");
        std::fs::write(&path, "x").unwrap();
        let Ok(file) = open_regular(&path) else { panic!("a regular file opens") };
        let flags = rustix::io::fcntl_getfd(&file).unwrap();
        assert!(flags.contains(rustix::io::FdFlags::CLOEXEC), "{flags:?}");
    }

    /// A change to a receipt's JSON before it is written.
    type Edit = Box<dyn FnOnce(&mut Value)>;

    /// A folder with evidence and a receipt for HEAD and `cli` that passes; `edit` changes the
    /// receipt's JSON before it is written. The folder, and the outputs that name the receipt.
    fn receipt(edit: impl FnOnce(&mut Value)) -> (tempfile::TempDir, Vec<u8>) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("run.log"), "drove it\n").unwrap();
        let mut v = json!({
            "schema": SCHEMA,
            "head": HEAD,
            "surface": "cli",
            "entry": "maestro flow next",
            "driver": "shell",
            "evidence": { "path": "run.log", "sha256": sha256(b"drove it\n") },
            "result": "pass",
            "at": "2026-11-11T12:00:00Z",
        });
        edit(&mut v);
        let path = tmp.path().join("receipt.json");
        std::fs::write(&path, v.to_string()).unwrap();
        let named = named(&path);
        (tmp, named)
    }

    /// The outputs naming the file at `path` with its digest as it is now; only a regular file is
    /// read for it (a FIFO would block the test, never the check).
    fn named(path: &Path) -> Vec<u8> {
        let regular = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file());
        let digest = if regular { sha256(&std::fs::read(path).unwrap()) } else { sha256(b"") };
        json!({ "receipt": path, "receipt_sha256": digest }).to_string().into_bytes()
    }

    fn failure(edit: impl FnOnce(&mut Value)) -> Failure {
        let (_tmp, input) = receipt(edit);
        verify(&input, HEAD, "cli").unwrap_err()
    }

    #[test]
    fn a_receipt_bound_to_the_head_and_surface_with_its_evidence_is_accepted() {
        let (tmp, input) = receipt(|_| {});
        let ok = verify(&input, HEAD, "cli").unwrap();
        assert_eq!(ok.evidence, tmp.path().join("run.log"));
        assert_eq!(ok.evidence_sha256, sha256(b"drove it\n"));
        assert_eq!(ok.receipt_sha256, sha256(&std::fs::read(tmp.path().join("receipt.json")).unwrap()));
        // Other outputs are let be.
        let mut more: Value = serde_json::from_slice(&input).unwrap();
        more["notes"] = json!("tried the CLI");
        assert!(verify(more.to_string().as_bytes(), HEAD, "cli").is_ok());
        // Evidence by an absolute path is read where it says.
        let elsewhere = tempfile::tempdir().unwrap();
        let log = elsewhere.path().join("abs.log");
        std::fs::write(&log, "drove it\n").unwrap();
        let (_tmp, input) = receipt(|v| v["evidence"]["path"] = json!(log));
        assert_eq!(verify(&input, HEAD, "cli").unwrap().evidence, log);
    }

    #[test]
    fn each_failure_has_its_own_code_and_exit_status() {
        let (tmp, input) = receipt(|_| {});
        let link = tmp.path().join("link.json");
        std::os::unix::fs::symlink(tmp.path().join("receipt.json"), &link).unwrap();
        let changed: Value = serde_json::from_slice(&input).unwrap();
        let changed = json!({ "receipt": changed["receipt"], "receipt_sha256": sha256(b"another receipt") });
        let failures = [
            verify(b"{}", HEAD, "cli").unwrap_err(),
            verify(&named(&tmp.path().join("none.json")), HEAD, "cli").unwrap_err(),
            verify(&named(tmp.path()), HEAD, "cli").unwrap_err(),
            verify(&named(&link), HEAD, "cli").unwrap_err(),
            verify(&named(Path::new("/dev/null")), HEAD, "cli").unwrap_err(),
            verify(changed.to_string().as_bytes(), HEAD, "cli").unwrap_err(),
            failure(|v| v["schema"] = json!("dogfood-receipt@2")),
            verify(&input, "0000000000000000000000000000000000000000", "cli").unwrap_err(),
            verify(&input, HEAD, "web").unwrap_err(),
            failure(|v| v["result"] = json!("fail")),
            failure(|v| v["evidence"] = json!({ "path": "/dev/null", "sha256": sha256(b"") })),
            failure(|v| v["evidence"]["path"] = json!("gone.log")),
            failure(|v| v["evidence"]["sha256"] = json!(sha256(b"something else"))),
        ];
        let codes: Vec<(&str, u8)> = failures.iter().map(|f| (f.code(), f.exit())).collect();
        assert_eq!(
            codes,
            [
                ("outputs_invalid", 11),
                ("receipt_missing", 3),
                ("receipt_not_regular", 13),
                ("receipt_not_regular", 13),
                ("receipt_not_regular", 13),
                ("receipt_changed", 12),
                ("receipt_malformed", 5),
                ("wrong_head", 6),
                ("wrong_surface", 7),
                ("result_not_pass", 8),
                ("evidence_not_regular", 14),
                ("evidence_unreadable", 9),
                ("evidence_mismatch", 10),
            ]
        );
        assert_eq!(
            failures[3].to_string(),
            format!("the receipt at {} is not a regular file: it is a symbolic link", link.display())
        );
        assert_eq!(
            failures[7].to_string(),
            format!("the receipt is for head {HEAD}, not 0000000000000000000000000000000000000000")
        );
        assert_eq!(failures[8].to_string(), "the receipt is for surface cli, not web");
        // Evidence through a link is refused too; a folder is not a regular file either.
        std::os::unix::fs::symlink(tmp.path().join("run.log"), tmp.path().join("run-link.log")).unwrap();
        for path in [tmp.path().join("run-link.log"), tmp.path().to_path_buf()] {
            let code = failure(|v| v["evidence"] = json!({ "path": path, "sha256": sha256(b"drove it\n") })).code();
            assert_eq!(code, "evidence_not_regular", "{path:?}");
        }
    }

    /// `check`'s result, or a failed test after 5 s: a check that hangs fails here, never stalls.
    fn within<T: Send + 'static>(check: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(check());
        });
        rx.recv_timeout(std::time::Duration::from_secs(5)).expect("the check hung")
    }

    #[test]
    fn a_fifo_with_no_writer_fails_at_once_and_never_hangs_the_check() {
        // A FIFO no one writes to: opening it to read would wait for a writer forever.
        let (tmp, _) = receipt(|_| {});
        let fifo = tmp.path().join("fifo");
        assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        let input = named(&fifo);
        let e = within(move || verify(&input, HEAD, "cli").unwrap_err());
        assert_eq!((e.code(), e.exit()), ("receipt_not_regular", 13), "{e}");
        let (_t, input) = receipt(|v| v["evidence"] = json!({ "path": fifo, "sha256": sha256(b"") }));
        let e = within(move || verify(&input, HEAD, "cli").unwrap_err());
        assert_eq!((e.code(), e.exit()), ("evidence_not_regular", 14), "{e}");
    }

    #[test]
    fn the_outputs_must_name_a_receipt_and_its_digest() {
        let (tmp, input) = receipt(|_| {});
        let path = tmp.path().join("receipt.json");
        let digest = sha256(&std::fs::read(&path).unwrap());
        for bad in [
            json!("receipt.json"),
            json!({ "receipt": path }),
            json!({ "receipt_sha256": digest }),
            json!({ "receipt": "", "receipt_sha256": digest }),
            json!({ "receipt": path, "receipt_sha256": digest.trim_start_matches("sha256:") }),
            json!({ "receipt": path, "receipt_sha256": digest.to_uppercase() }),
            json!({ "receipt": "r".repeat(MAX_FIELD + 1), "receipt_sha256": digest }),
        ] {
            assert_eq!(verify(bad.to_string().as_bytes(), HEAD, "cli").unwrap_err().code(), "outputs_invalid", "{bad}");
        }
        // At the bound the input is read; one byte over it is not.
        let mut padded = input.clone();
        padded.resize(MAX_OUTPUTS, b' ');
        assert!(verify(&padded, HEAD, "cli").is_ok());
        padded.push(b' ');
        assert_eq!(verify(&padded, HEAD, "cli").unwrap_err().code(), "outputs_invalid");
    }

    #[test]
    fn a_receipt_changed_after_it_was_named_no_longer_matches() {
        // The digest in the outputs, which the kernel logs, pins the receipt's content.
        let (tmp, input) = receipt(|_| {});
        assert!(verify(&input, HEAD, "cli").is_ok());
        let path = tmp.path().join("receipt.json");
        let mut v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        v["entry"] = json!("something else");
        std::fs::write(&path, v.to_string()).unwrap();
        assert_eq!(verify(&input, HEAD, "cli").unwrap_err().code(), "receipt_changed");
    }

    #[test]
    fn a_receipt_that_is_not_well_formed_is_malformed() {
        let cases: [(&str, Edit); 12] = [
            ("not json", Box::new(|v| *v = json!("receipt"))),
            ("unknown field", Box::new(|v| v["verdict"] = json!("pass"))),
            ("unknown evidence field", Box::new(|v| v["evidence"]["extra"] = json!(1))),
            ("no head", Box::new(|v| drop(v.as_object_mut().unwrap().remove("head")))),
            ("empty entry", Box::new(|v| v["entry"] = json!(" "))),
            ("empty driver", Box::new(|v| v["driver"] = json!(""))),
            ("empty evidence", Box::new(|v| v["evidence"]["path"] = json!(""))),
            ("long surface", Box::new(|v| v["surface"] = json!("s".repeat(MAX_FIELD + 1)))),
            ("bare digest", Box::new(|v| v["evidence"]["sha256"] = json!("ab".repeat(32)))),
            ("upper digest", Box::new(|v| v["evidence"]["sha256"] = json!(format!("sha256:{}", "AB".repeat(32))))),
            ("result", Box::new(|v| v["result"] = json!("passed"))),
            ("at", Box::new(|v| v["at"] = json!("yesterday"))),
        ];
        for (name, edit) in cases {
            assert_eq!(failure(edit).code(), "receipt_malformed", "{name}");
        }
        // At the bound a field is taken; one byte over it is not.
        let (_tmp, input) = receipt(|v| v["surface"] = json!("s".repeat(MAX_FIELD)));
        assert_eq!(verify(&input, HEAD, &"s".repeat(MAX_FIELD)).unwrap().surface.len(), MAX_FIELD);
    }

    #[test]
    fn a_receipt_over_its_bound_is_unreadable_and_one_at_it_is_read() {
        let (tmp, _) = receipt(|_| {});
        let path = tmp.path().join("receipt.json");
        let text = std::fs::read_to_string(&path).unwrap();
        // Whitespace pads the JSON to the bound exactly, then one byte past it.
        let at = format!("{text}{}", " ".repeat(MAX_RECEIPT - text.len()));
        std::fs::write(&path, &at).unwrap();
        assert!(verify(&named(&path), HEAD, "cli").is_ok());
        std::fs::write(&path, format!("{at} ")).unwrap();
        assert_eq!(verify(&named(&path), HEAD, "cli").unwrap_err().code(), "receipt_unreadable");
    }

    #[test]
    fn evidence_over_its_bound_is_unreadable_and_one_at_it_is_read() {
        // The bound, at a size a test can read: at it the file is hashed, one byte past it not.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("e.bin");
        std::fs::write(&path, b"12345678").unwrap();
        assert_eq!(hash_bounded(&path, 8).unwrap(), sha256(b"12345678"));
        let e = hash_bounded(&path, 7).unwrap_err();
        assert_eq!((e.code(), e.exit()), ("evidence_unreadable", 9), "{e}");
        assert_eq!(MAX_EVIDENCE, 1 << 30);
    }
}
