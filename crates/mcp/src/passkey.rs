//! The owner's passkey (BRO-4042; owner decision 2026-11-20, the spec's owner principal).
//!
//! An owner verb is accepted only with a WebAuthn assertion, made with a passkey the owner
//! enrolled, over a challenge that commits to the verb and its content:
//! `challenge = SHA-256(intent || "\n" || nonce)`, where `intent` is the verb's canonical JSON.
//! Anyone holding the intent, the nonce and the assertion can check it again against the
//! enrolled public key, so the record of an answer carries its own proof.
//!
//! Only ES256 (P-256) passkeys, the kind Apple's platform authenticator makes. An assertion is
//! accepted only with user presence and user verification (Touch ID or Face ID), from the
//! configured origin, for its relying party.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{DerSignature, VerifyingKey};
use p256::pkcs8::DecodePublicKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// COSE's number for ES256.
pub const ES256: i64 = -7;

/// Where the passkeys live: `MAESTRO_OWNER_DIR`, else `~/.local/state/maestro/owner`.
pub fn owner_dir() -> PathBuf {
    match std::env::var_os("MAESTRO_OWNER_DIR").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => std::env::home_dir().unwrap_or_default().join(".local/state/maestro/owner"),
    }
}

/// The relying party: the page's origin (`https://host` or `http://localhost:port`) and its id (the host).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Party {
    pub origin: String,
    pub id: String,
    /// The host and port a request to the origin names in `Host`: lowercase, with no default port.
    pub authority: String,
}

impl Party {
    /// From an origin. Only https, or http on localhost (a secure context), carries a passkey.
    pub fn parse(origin: &str) -> Result<Self, String> {
        let (scheme, rest) = origin.split_once("://").ok_or_else(|| format!("{origin:?} is not an origin"))?;
        if rest.is_empty() || rest.contains(['/', '?', '#', '@']) {
            return Err(format!("{origin:?} is not an origin"));
        }
        let host = match rest.rsplit_once(':') {
            Some((h, port)) if !h.ends_with(']') && port.bytes().all(|b| b.is_ascii_digit()) => h,
            _ => rest,
        };
        match scheme {
            "https" => {}
            "http" if host == "localhost" => {}
            _ => return Err(format!("{origin:?}: a passkey needs https, or http on localhost")),
        }
        let authority = authority_of(rest, scheme == "https");
        Ok(Self { origin: origin.to_string(), id: host.to_ascii_lowercase(), authority })
    }
}

/// A `Host` value as a comparable authority: lowercase, one trailing dot of the name dropped, and
/// the scheme's default port (443 for https, else 80) dropped, as a browser omits it.
pub fn authority_of(value: &str, https: bool) -> String {
    let v = value.to_ascii_lowercase();
    let (host, port) = match v.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || h.starts_with('[') && !p.contains(']') => {
            (h.to_string(), Some(p.to_string()))
        }
        _ => (v.clone(), None),
    };
    let host = host.strip_suffix('.').unwrap_or(&host).to_string();
    match port.as_deref() {
        Some(p) if p == if https { "443" } else { "80" } => host,
        Some(p) => format!("{host}:{p}"),
        None => host,
    }
}

/// One enrolled passkey.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credential {
    /// The credential id, base64url.
    pub id: String,
    /// The public key, SPKI DER in base64.
    pub public_key: String,
    pub enrolled_at: String,
    /// The relying party it was made for.
    pub rp_id: String,
}

/// The enrolled passkeys, as a file. A file that does not read is an error, never an empty list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keyring {
    pub credentials: Vec<Credential>,
}

impl Keyring {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join("passkeys.json")
    }

    pub fn load(dir: &Path) -> Result<Self, String> {
        match std::fs::read(Self::path(dir)) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("passkeys.json: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("passkeys.json: {e}")),
        }
    }

    /// Written whole to a new file, then renamed over the old one, mode 0600.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let tmp = dir.join(".passkeys.json.tmp");
        let body = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)
                .map_err(|e| e.to_string())?;
            f.write_all(&body).map_err(|e| e.to_string())?;
            f.sync_all().map_err(|e| e.to_string())?;
        }
        std::fs::rename(&tmp, Self::path(dir)).map_err(|e| e.to_string())
    }

    pub fn find(&self, id: &str) -> Option<&Credential> {
        self.credentials.iter().find(|c| c.id == id)
    }
}

/// A fresh random value, base64url.
pub fn nonce() -> String {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("the system's random source");
    URL_SAFE_NO_PAD.encode(b)
}

/// The challenge an intent and a nonce commit to, base64url as WebAuthn carries it.
pub fn challenge(intent: &str, nonce: &str) -> String {
    let mut h = Sha256::new();
    h.update(intent.as_bytes());
    h.update(b"\n");
    h.update(nonce.as_bytes());
    URL_SAFE_NO_PAD.encode(h.finalize())
}

/// An assertion as the page sends it: every binary field base64url.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assertion {
    pub credential: String,
    pub client_data: String,
    pub authenticator_data: String,
    pub signature: String,
}

/// A new passkey as the page sends it: its id, its SPKI public key and algorithm (from
/// `getPublicKey()` and `getPublicKeyAlgorithm()`), its client data and authenticator data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    pub credential: String,
    pub public_key: String,
    pub algorithm: i64,
    pub client_data: String,
    pub authenticator_data: String,
}

fn b64url(field: &str, v: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD.decode(v.trim_end_matches('=')).map_err(|_| format!("{field} is not base64url"))
}

#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
    #[serde(rename = "crossOrigin", default)]
    cross_origin: bool,
}

/// The checks client data and authenticator data share: the ceremony, the challenge, the
/// origin, the relying party, and the owner present and verified.
fn check(party: &Party, kind: &str, challenge: &str, client_data: &[u8], auth: &[u8]) -> Result<(), String> {
    let cd: ClientData =
        serde_json::from_slice(client_data).map_err(|_| "the client data does not read".to_string())?;
    if cd.kind != kind {
        return Err(format!("not a {kind} ceremony"));
    }
    if cd.challenge.trim_end_matches('=') != challenge {
        return Err("the passkey signed another request".into());
    }
    if cd.origin != party.origin || cd.cross_origin {
        return Err(format!("signed for {} , not {}", cd.origin, party.origin));
    }
    if auth.len() < 37 {
        return Err("the authenticator data is too short".into());
    }
    if auth[..32] != Sha256::digest(party.id.as_bytes())[..] {
        return Err("signed for another relying party".into());
    }
    let flags = auth[32];
    if flags & 0x01 == 0 {
        return Err("the owner was not present".into());
    }
    if flags & 0x04 == 0 {
        return Err("the owner was not verified (Touch ID or Face ID)".into());
    }
    Ok(())
}

/// A registration checked: the credential to enroll.
pub fn register(party: &Party, challenge: &str, r: &Registration, at: &str) -> Result<Credential, String> {
    if r.algorithm != ES256 {
        return Err("only ES256 passkeys are enrolled".into());
    }
    check(
        party,
        "webauthn.create",
        challenge,
        &b64url("client data", &r.client_data)?,
        &b64url("authenticator data", &r.authenticator_data)?,
    )?;
    let spki = b64url("public key", &r.public_key)?;
    VerifyingKey::from_public_key_der(&spki).map_err(|_| "the public key is not a P-256 key".to_string())?;
    b64url("credential", &r.credential)?;
    Ok(Credential {
        id: r.credential.clone(),
        public_key: STANDARD.encode(spki),
        enrolled_at: at.to_string(),
        rp_id: party.id.clone(),
    })
}

/// An assertion checked against the keyring, for `challenge`. Returns the credential that signed.
pub fn verify<'k>(
    party: &Party,
    keyring: &'k Keyring,
    challenge: &str,
    a: &Assertion,
) -> Result<&'k Credential, String> {
    let cred = keyring.find(&a.credential).ok_or("that passkey is not enrolled")?;
    if cred.rp_id != party.id {
        return Err("that passkey was enrolled for another host".into());
    }
    let client_data = b64url("client data", &a.client_data)?;
    let auth = b64url("authenticator data", &a.authenticator_data)?;
    check(party, "webauthn.get", challenge, &client_data, &auth)?;
    let spki = STANDARD.decode(&cred.public_key).map_err(|_| "the enrolled key does not read".to_string())?;
    let key = VerifyingKey::from_public_key_der(&spki).map_err(|_| "the enrolled key does not read".to_string())?;
    let sig = DerSignature::from_bytes(&b64url("signature", &a.signature)?)
        .map_err(|_| "the signature does not read".to_string())?;
    let mut signed = auth.clone();
    signed.extend_from_slice(&Sha256::digest(&client_data));
    key.verify(&signed, &sig).map_err(|_| "the signature does not verify".to_string())?;
    Ok(cred)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use p256::ecdsa::SigningKey;
    use p256::ecdsa::signature::Signer;

    pub(crate) fn party() -> Party {
        Party::parse("https://mac.example.ts.net").unwrap()
    }

    /// A P-256 key's SPKI DER: the fixed header, then the uncompressed point.
    fn spki_of(k: &SigningKey) -> Vec<u8> {
        let mut der = vec![
            0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a, 0x86, 0x48,
            0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
        ];
        der.extend_from_slice(k.verifying_key().to_encoded_point(false).as_bytes());
        der
    }

    pub(crate) fn key() -> SigningKey {
        SigningKey::from_slice(&[7u8; 32]).unwrap()
    }

    fn auth_data(party: &Party, flags: u8) -> Vec<u8> {
        let mut a = Sha256::digest(party.id.as_bytes()).to_vec();
        a.push(flags);
        a.extend_from_slice(&[0, 0, 0, 1]);
        a
    }

    fn client(kind: &str, challenge: &str, origin: &str) -> String {
        URL_SAFE_NO_PAD
            .encode(format!(r#"{{"type":"{kind}","challenge":"{challenge}","origin":"{origin}","crossOrigin":false}}"#))
    }

    pub(crate) fn keyring() -> Keyring {
        let spki = spki_of(&key());
        Keyring {
            credentials: vec![Credential {
                id: "Y3JlZA".into(),
                public_key: STANDARD.encode(&spki),
                enrolled_at: "t".into(),
                rp_id: party().id,
            }],
        }
    }

    /// What Touch ID returns for `challenge`, with these flags and this origin.
    pub(crate) fn sign_with(challenge: &str, flags: u8, origin: &str, rp: &Party) -> Assertion {
        sign_kind("webauthn.get", challenge, flags, origin, rp)
    }

    /// A validly signed assertion whose client data names ceremony `kind`.
    fn sign_kind(kind: &str, challenge: &str, flags: u8, origin: &str, rp: &Party) -> Assertion {
        let cd = client(kind, challenge, origin);
        let auth = auth_data(rp, flags);
        let mut signed = auth.clone();
        signed.extend_from_slice(&Sha256::digest(URL_SAFE_NO_PAD.decode(&cd).unwrap()));
        let sig: p256::ecdsa::Signature = key().sign(&signed);
        Assertion {
            credential: "Y3JlZA".into(),
            client_data: cd,
            authenticator_data: URL_SAFE_NO_PAD.encode(auth),
            signature: URL_SAFE_NO_PAD.encode(sig.to_der().as_bytes()),
        }
    }

    pub(crate) fn sign(challenge: &str) -> Assertion {
        sign_with(challenge, 0x05, &party().origin, &party())
    }

    #[test]
    fn an_assertion_over_the_challenge_verifies() {
        let c = challenge("{\"verb\":\"ask.answer\"}", "n1");
        assert!(verify(&party(), &keyring(), &c, &sign(&c)).is_ok());
    }

    #[test]
    fn every_forgery_is_refused() {
        let p = party();
        let k = keyring();
        let c = challenge("{\"verb\":\"ask.answer\",\"option\":\"a\"}", "n1");
        // Another intent's signature.
        let other = challenge("{\"verb\":\"ask.answer\",\"option\":\"b\"}", "n1");
        assert!(verify(&p, &k, &c, &sign(&other)).is_err(), "another intent");
        // No user verification, no presence.
        assert!(verify(&p, &k, &c, &sign_with(&c, 0x01, &p.origin, &p)).is_err(), "not verified");
        assert!(verify(&p, &k, &c, &sign_with(&c, 0x04, &p.origin, &p)).is_err(), "not present");
        // Another origin, another relying party.
        assert!(verify(&p, &k, &c, &sign_with(&c, 0x05, "https://evil.example", &p)).is_err(), "origin");
        let evil = Party::parse("https://evil.example").unwrap();
        assert!(verify(&p, &k, &c, &sign_with(&c, 0x05, &p.origin, &evil)).is_err(), "rp");
        // A tampered signature, or authenticator data changed after signing.
        let mut a = sign(&c);
        a.authenticator_data = URL_SAFE_NO_PAD.encode(auth_data(&p, 0x45));
        assert!(verify(&p, &k, &c, &a).is_err(), "tampered auth data");
        let mut a = sign(&c);
        a.credential = "b3RoZXI".into();
        assert!(verify(&p, &k, &c, &a).is_err(), "unknown credential");
        // A key that is not enrolled.
        let mut a = sign(&c);
        let stranger = SigningKey::from_slice(&[9u8; 32]).unwrap();
        let mut signed = URL_SAFE_NO_PAD.decode(&a.authenticator_data).unwrap();
        signed.extend_from_slice(&Sha256::digest(URL_SAFE_NO_PAD.decode(&a.client_data).unwrap()));
        let sig: p256::ecdsa::Signature = stranger.sign(&signed);
        a.signature = URL_SAFE_NO_PAD.encode(sig.to_der().as_bytes());
        assert!(verify(&p, &k, &c, &a).is_err(), "stranger");
        // A registration's client data, validly signed, is not an assertion.
        assert!(verify(&p, &k, &c, &sign_kind("webauthn.create", &c, 0x05, &p.origin, &p)).is_err(), "signed create");
        // A get assertion is not a registration, and the other way round.
        let mut a = sign(&c);
        a.client_data = client("webauthn.create", &c, &p.origin);
        assert!(verify(&p, &k, &c, &a).is_err(), "ceremony");
    }

    #[test]
    fn a_registration_enrolls_only_es256_for_this_party() {
        let p = party();
        let c = challenge("enroll", "n");
        let r = Registration {
            credential: "Y3JlZA".into(),
            public_key: URL_SAFE_NO_PAD.encode(spki_of(&key())),
            algorithm: ES256,
            client_data: client("webauthn.create", &c, &p.origin),
            authenticator_data: URL_SAFE_NO_PAD.encode(auth_data(&p, 0x45)),
        };
        let cred = register(&p, &c, &r, "t").unwrap();
        assert_eq!(cred.rp_id, p.id);
        assert!(register(&p, &c, &Registration { algorithm: -257, ..r.clone() }, "t").is_err());
        assert!(register(&p, &challenge("other", "n"), &r, "t").is_err());
        assert!(
            register(
                &p,
                &c,
                &Registration { authenticator_data: URL_SAFE_NO_PAD.encode(auth_data(&p, 0x41)), ..r },
                "t"
            )
            .is_err()
        );
    }

    #[test]
    fn the_authority_keeps_the_port_but_not_the_default_one() {
        assert_eq!(Party::parse("https://Mac.example.ts.net:7443").unwrap().authority, "mac.example.ts.net:7443");
        assert_eq!(Party::parse("https://mac.example.ts.net").unwrap().authority, "mac.example.ts.net");
        assert_eq!(authority_of("MAC.example.ts.net.:443", true), "mac.example.ts.net");
        assert_eq!(authority_of("localhost:8181", false), "localhost:8181");
        assert_ne!(authority_of("localhost:8181", false), Party::parse("http://localhost:7171").unwrap().authority);
    }

    #[test]
    fn only_a_secure_origin_is_a_party() {
        assert_eq!(Party::parse("http://localhost:7181").unwrap().id, "localhost");
        assert_eq!(Party::parse("https://Mac.example.ts.net").unwrap().id, "mac.example.ts.net");
        for bad in ["http://100.82.213.74:7171", "https://x/y", "x", "https://u@x"] {
            assert!(Party::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_keyring_round_trips_and_an_unreadable_one_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(Keyring::load(d.path()).unwrap(), Keyring::default());
        keyring().save(d.path()).unwrap();
        assert_eq!(Keyring::load(d.path()).unwrap(), keyring());
        std::fs::write(Keyring::path(d.path()), "{").unwrap();
        assert!(Keyring::load(d.path()).is_err());
    }
}
