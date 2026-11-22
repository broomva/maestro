//! The owner passkey implementation is shared with the flow kernel.
pub use maestro_store::passkey::*;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
    use p256::ecdsa::SigningKey;
    use p256::ecdsa::signature::Signer;
    use sha2::{Digest, Sha256};

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
