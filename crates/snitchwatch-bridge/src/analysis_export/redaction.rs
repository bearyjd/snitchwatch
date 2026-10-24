//! Per-export keyed identities. Neither raw identities nor full digests are serialized.

use super::snapshot::AppIdentity;
use super::ExportError;
use ring::{hmac, rand::SystemRandom};
use std::collections::HashMap;
use std::net::IpAddr;

type Digest = [u8; 32];

pub fn fresh_key() -> Result<hmac::Key, ExportError> {
    hmac::Key::generate(hmac::HMAC_SHA256, &SystemRandom::new()).map_err(|_| ExportError::Internal)
}

/// Every tuple element, including the domain and tag, has an unsigned 64-bit
/// big-endian byte length. Versioning is outside the tuple and fixed here.
pub(super) fn digest(key: &hmac::Key, domain: &[u8], parts: &[&[u8]]) -> Digest {
    let mut context = hmac::Context::with_key(key);
    context.update(b"snitchwatch-cache-summary\0redaction-v1");
    for part in std::iter::once(domain).chain(parts.iter().copied()) {
        context.update(&(part.len() as u64).to_be_bytes());
        context.update(part);
    }
    let mut digest = [0; 32];
    digest.copy_from_slice(context.sign().as_ref());
    digest
}

pub(super) struct RedactedIdentity {
    pub digest: Digest,
    pub reference: String,
}

pub(super) struct Redactor<'a> {
    key: &'a hmac::Key,
    labels: HashMap<(u8, [u8; 16]), Digest>,
}

impl<'a> Redactor<'a> {
    pub fn new(key: &'a hmac::Key) -> Self {
        Self {
            key,
            labels: HashMap::new(),
        }
    }

    pub fn app(&mut self, app: &AppIdentity) -> Result<RedactedIdentity, ExportError> {
        let parts: [&[u8]; 2] = match app {
            AppIdentity::Path { value } => [b"path", value.as_bytes()],
            AppIdentity::MissingPath { process } => [b"missing-path", process.as_bytes()],
        };
        let digest = digest(self.key, b"app", &parts);
        self.register(b'a', digest)
    }

    pub fn destination(&mut self, value: &str) -> Result<RedactedIdentity, ExportError> {
        let digest = match value.parse::<IpAddr>() {
            Ok(IpAddr::V4(ip)) => digest(self.key, b"destination", &[b"ipv4", &ip.octets()]),
            Ok(IpAddr::V6(ip)) => digest(self.key, b"destination", &[b"ipv6", &ip.octets()]),
            Err(_) => digest(self.key, b"destination", &[b"hostname", value.as_bytes()]),
        };
        self.register(b'd', digest)
    }

    pub(super) fn register(
        &mut self,
        domain: u8,
        digest: Digest,
    ) -> Result<RedactedIdentity, ExportError> {
        let mut short = [0; 16];
        short.copy_from_slice(&digest[..16]);
        match self.labels.get(&(domain, short)) {
            Some(existing) if existing != &digest => return Err(ExportError::Collision),
            Some(_) => (),
            None => {
                self.labels.insert((domain, short), digest);
            }
        }
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut reference = String::with_capacity(34);
        reference.push(char::from(domain));
        reference.push('-');
        for byte in short {
            reference.push(char::from(HEX[usize::from(byte >> 4)]));
            reference.push(char::from(HEX[usize::from(byte & 15)]));
        }
        Ok(RedactedIdentity { digest, reference })
    }
}

pub(super) fn app_basename(app: &AppIdentity) -> Option<String> {
    let AppIdentity::Path { value } = app else {
        return None;
    };
    let allowed = ["/usr/", "/opt/", "/app/", "/bin/", "/sbin/"];
    (crate::translator::process_binding::is_bindable_process_path(value)
        && !value.ends_with(" (deleted)")
        && allowed.iter().any(|prefix| value.starts_with(prefix)))
    .then(|| value.rsplit('/').next().unwrap_or_default().to_owned())
}
