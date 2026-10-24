use super::redaction::{digest, fresh_key, Redactor};
use super::snapshot::AppIdentity;
use super::ExportError;
use ring::hmac::{self, Key};

fn key() -> Key {
    Key::new(hmac::HMAC_SHA256, &[7; 32])
}

#[test]
fn redaction_domains_freshness_and_collision() {
    let fixed_key = key();
    let mut redactor = Redactor::new(&fixed_key);
    let app = redactor
        .app(&AppIdentity::Path {
            value: "/usr/bin/example".into(),
        })
        .unwrap();
    let destination = redactor.destination("example.invalid").unwrap();
    // Independent Python stdlib hmac SHA-256 vectors, including version/domain/length encoding.
    assert_eq!(app.reference, "a-4c9d96b0c8f92ac1ceb4d2ebfdaf6153");
    assert_eq!(destination.reference, "d-be82e422603d50a044b6aba697f5b5cf");
    assert_eq!(app.digest.len(), 32);
    let same_path = redactor
        .app(&AppIdentity::Path {
            value: "same".into(),
        })
        .unwrap();
    let same_missing = redactor
        .app(&AppIdentity::MissingPath {
            process: "same".into(),
        })
        .unwrap();
    let same_destination = redactor.destination("same").unwrap();
    assert_ne!(same_path.digest, same_missing.digest);
    assert_ne!(same_path.digest, same_destination.digest);
    assert_ne!(same_missing.digest, same_destination.digest);
    for reference in [&app.reference, &destination.reference] {
        assert_eq!(reference.len(), 34);
        assert!(reference.as_bytes()[2..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)));
    }
    assert_ne!(
        digest(&key(), b"app", &[b"path", b"same"]),
        digest(&key(), b"destination", &[b"path", b"same"])
    );
    assert_ne!(
        digest(&key(), b"app", &[b"ab", b"c"]),
        digest(&key(), b"app", &[b"a", b"bc"])
    );
    assert_ne!(
        digest(&key(), b"app", &[b"a", b"b"]),
        digest(&key(), b"app", &[b"a", b"b", b""])
    );
    assert_ne!(
        digest(&fresh_key().unwrap(), b"app", &[b"same"]),
        digest(&fresh_key().unwrap(), b"app", &[b"same"])
    );
    let mut first = [0; 32];
    first[31] = 1;
    let mut second = first;
    second[31] = 2;
    assert_ne!(first, second);
    assert!(redactor.register(b'a', first).is_ok());
    assert_eq!(
        redactor.register(b'a', second).err(),
        Some(ExportError::Collision)
    );
    assert!(redactor.register(b'a', first).is_ok());
    assert!(redactor.register(b'd', second).is_ok());
}
