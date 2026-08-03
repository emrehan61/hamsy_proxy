//! Certificate authority: generates/loads a root CA and mints per-host leaf
//! certificates on demand for MITM'ing TLS connections.
//!
//! Design notes:
//! - We reuse a single ECDSA leaf keypair for every minted host certificate
//!   (only the certificate, signed by the CA, differs per host). This makes
//!   minting a pure signing operation with no key generation on the hot
//!   path, which is what keeps interactive MITM latency low.
//! - Minted certs (and the `rustls::ServerConfig`s built from them) are
//!   cached in a small hand-rolled LRU keyed by hostname, since minting
//!   still costs a signature and building a `ServerConfig` re-validates the
//!   cert/key pair.
//! - Every public entry point that takes a hostname (ultimately sourced from
//!   a hostile client's SNI) validates/bounds it defensively; nothing here
//!   panics on adversarial input.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SerialNumber, PKCS_ECDSA_P256_SHA256,
};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::error::{ProxyError, Result};

/// Maximum number of minted host certificates kept in the LRU cache.
const LEAF_CACHE_CAPACITY: usize = 1024;
/// Maximum accepted length for a hostname passed into [`CertAuthority::mint`]
/// or [`CertAuthority::server_config`]; anything longer is rejected instead
/// of being handed to the ASN.1/cert-building machinery.
const MAX_HOST_LEN: usize = 255;

const CA_CERT_FILE: &str = "ca.pem";
const CA_KEY_FILE: &str = "ca-key.pem";

/// A minted leaf certificate chain plus the (shared, reused-across-hosts)
/// private key it was signed with.
pub struct MintedCert {
    /// The leaf certificate (and, in principle, any intermediates - here
    /// just the single leaf, since our chain is only two levels deep and
    /// the client is expected to trust the root directly).
    pub cert_chain: Vec<CertificateDer<'static>>,
    /// The leaf's private key.
    pub key: PrivateKeyDer<'static>,
}

fn cert_err(e: rcgen::Error) -> ProxyError {
    ProxyError::Cert(e.to_string())
}

/// Validates/normalizes a hostname before it is used to mint a certificate.
///
/// Strips IPv6 literal brackets (`[::1]` -> `::1`), rejects empty or
/// pathologically long input. This is the single choke point protecting the
/// cert-minting path from adversarial SNI values; everything downstream may
/// assume a small, non-empty string.
fn sanitize_host(host: &str) -> Result<String> {
    let trimmed = host.trim();
    let stripped = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    if stripped.is_empty() {
        return Err(ProxyError::InvalidTarget("empty host".to_string()));
    }
    if stripped.len() > MAX_HOST_LEN {
        return Err(ProxyError::InvalidTarget("host name too long".to_string()));
    }
    Ok(stripped.to_string())
}

/// Restricts `offered` (the ALPN protocols a client presented in its
/// ClientHello) to the subset rdproxy actually understands (`h2`,
/// `http/1.1`), preserving the client's preference order.
fn restrict_alpn(offered: &[Vec<u8>]) -> Vec<Vec<u8>> {
    offered
        .iter()
        .filter(|p| p.as_slice() == b"h2" || p.as_slice() == b"http/1.1")
        .cloned()
        .collect()
}

fn build_server_config(minted: &MintedCert, alpn: &[Vec<u8>]) -> Result<Arc<rustls::ServerConfig>> {
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(minted.cert_chain.clone(), minted.key.clone_key())
        .map_err(|e| ProxyError::Cert(e.to_string()))?;
    config.alpn_protocols = alpn.to_vec();
    Ok(Arc::new(config))
}

/// Per-host cache entry: the minted cert plus any `ServerConfig`s already
/// built from it (kept small since only a handful of distinct ALPN
/// combinations occur in practice: `[]`, `["http/1.1"]`, `["h2",
/// "http/1.1"]`).
struct HostEntry {
    minted: Arc<MintedCert>,
    configs: Vec<(Vec<Vec<u8>>, Arc<rustls::ServerConfig>)>,
}

/// A tiny hand-rolled LRU keyed by hostname. Capacity is small (1024) and
/// lookups are infrequent (once per TLS handshake, not per request), so a
/// linear scan for recency bookkeeping is an acceptable trade for simplicity.
struct HostCache {
    capacity: usize,
    map: HashMap<String, HostEntry>,
    order: VecDeque<String>,
}

impl HostCache {
    fn new(capacity: usize) -> Self {
        HostCache {
            capacity,
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn touch(&mut self, host: &str) {
        if let Some(pos) = self.order.iter().position(|h| h == host) {
            self.order.remove(pos);
        }
        self.order.push_back(host.to_string());
    }

    /// Returns the cache entry for `host`, minting (via `mint`) and
    /// inserting it first if absent. Evicts the least-recently-used entry
    /// once capacity is exceeded.
    fn get_or_mint(
        &mut self,
        host: &str,
        mint: impl FnOnce() -> Result<MintedCert>,
    ) -> Result<&mut HostEntry> {
        if self.map.contains_key(host) {
            self.touch(host);
        } else {
            let minted = Arc::new(mint()?);
            self.map.insert(
                host.to_string(),
                HostEntry {
                    minted,
                    configs: Vec::new(),
                },
            );
            self.order.push_back(host.to_string());
            while self.order.len() > self.capacity {
                if let Some(oldest) = self.order.pop_front() {
                    self.map.remove(&oldest);
                }
            }
        }
        // We just inserted or confirmed presence above under the same lock,
        // so this lookup cannot fail.
        self.map
            .get_mut(host)
            .ok_or_else(|| ProxyError::Other("cert cache invariant violated".to_string()))
    }
}

/// Certificate authority used to MITM TLS connections: holds (or generates)
/// a root CA and mints short-lived leaf certificates for whatever hostnames
/// clients connect through, all signed by that root.
pub struct CertAuthority {
    ca_pem: String,
    ca_der: CertificateDer<'static>,
    /// A reconstructed `rcgen::Certificate` used purely as the signing basis
    /// (distinguished name, key usages, key identifier method) for minting
    /// leaves; its own DER bytes are never exposed (`ca_der`/`ca_pem`, which
    /// are the literal on-disk bytes, are used for that instead).
    issuer: Certificate,
    ca_key: KeyPair,
    leaf_key: KeyPair,
    cache: Mutex<HostCache>,
}

impl CertAuthority {
    /// Loads a CA from `<dir>/ca.pem` + `<dir>/ca-key.pem`, generating and
    /// persisting a new one if either file is absent.
    pub fn load_or_generate(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let ca_path = dir.join(CA_CERT_FILE);
        let key_path = dir.join(CA_KEY_FILE);

        let (ca_pem, ca_key_pem) = if ca_path.is_file() && key_path.is_file() {
            (
                std::fs::read_to_string(&ca_path)?,
                std::fs::read_to_string(&key_path)?,
            )
        } else {
            let (pem, key_pem) = generate_ca_pem()?;
            write_secret_file(&ca_path, pem.as_bytes())?;
            write_secret_file(&key_path, key_pem.as_bytes())?;
            (pem, key_pem)
        };

        let ca_key = KeyPair::from_pem(&ca_key_pem).map_err(cert_err)?;
        let ca_der = first_cert_der(&ca_pem)?;
        // Reconstruct a signing-capable `Certificate` from the persisted PEM.
        // Its own serialized bytes may differ from what's on disk (re-signed
        // during reconstruction); that's fine, since we never expose them -
        // only `ca_pem`/`ca_der` (the literal stored bytes) are surfaced.
        let issuer = CertificateParams::from_ca_cert_pem(&ca_pem)
            .map_err(cert_err)?
            .self_signed(&ca_key)
            .map_err(cert_err)?;

        let leaf_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(cert_err)?;

        Ok(CertAuthority {
            ca_pem,
            ca_der,
            issuer,
            ca_key,
            leaf_key,
            cache: Mutex::new(HostCache::new(LEAF_CACHE_CAPACITY)),
        })
    }

    /// Mints (or returns a cached) leaf certificate for `host`.
    ///
    /// Concurrent callers for the same host never mint twice: the whole
    /// mint-and-insert sequence runs under a single, short-lived
    /// `parking_lot::Mutex` critical section (all synchronous CPU work, no
    /// `.await` inside), which both prevents redundant signing and makes
    /// deadlock structurally impossible.
    pub fn mint(&self, host: &str) -> Result<Arc<MintedCert>> {
        let host = sanitize_host(host)?;
        let mut cache = self.cache.lock();
        let entry = cache.get_or_mint(&host, || self.mint_uncached(&host))?;
        Ok(entry.minted.clone())
    }

    /// Builds (or returns a cached) `rustls::ServerConfig` for `host`,
    /// advertising only the ALPN protocols in `offered_alpn` that rdproxy
    /// supports (`h2`, `http/1.1`), preserving client preference order.
    pub fn server_config(
        &self,
        host: &str,
        offered_alpn: &[Vec<u8>],
    ) -> Result<Arc<rustls::ServerConfig>> {
        let host = sanitize_host(host)?;
        let restricted = restrict_alpn(offered_alpn);
        let mut cache = self.cache.lock();
        let entry = cache.get_or_mint(&host, || self.mint_uncached(&host))?;
        if let Some((_, cfg)) = entry.configs.iter().find(|(a, _)| *a == restricted) {
            return Ok(cfg.clone());
        }
        let cfg = build_server_config(&entry.minted, &restricted)?;
        entry.configs.push((restricted, cfg.clone()));
        Ok(cfg)
    }

    /// The actual (synchronous, CPU-only) signing operation, run while the
    /// cache lock is held by callers.
    fn mint_uncached(&self, host: &str) -> Result<MintedCert> {
        let mut params = CertificateParams::new(vec![host.to_string()]).map_err(cert_err)?;
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, host);
        params.distinguished_name = dn;
        // `ExplicitNoCa` (rather than `NoCa`) is what makes rcgen actually
        // *write* the Subject Key Identifier and Basic Constraints
        // extensions (with CA:FALSE); `NoCa` silently omits both. Without
        // Basic Constraints present, strict verifiers (OpenSSL 3.x, and by
        // extension Python/Java/Go/Node) reject the leaf.
        params.is_ca = IsCa::ExplicitNoCa;
        // digitalSignature covers ECDSA/EdDSA server auth; keyEncipherment
        // is only meaningful for RSA key exchange, but including it is
        // harmless for our ECDSA leaf key and matches what a real WebPKI
        // leaf carries.
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        // Authority Key Identifier: this is the extension that was actually
        // breaking strict verifiers (see module-level bug report). `self.issuer`
        // was reconstructed by `CertificateParams::from_ca_cert_pem` in
        // `load_or_generate`, which parses the Subject Key Identifier already
        // present in the on-disk `ca.pem` and stores it as
        // `KeyIdMethod::PreSpecified`. Setting this flag makes rcgen emit an
        // AKI keyid equal to that real, on-disk CA SKI - for both freshly
        // generated CAs and pre-existing ones - without us hand-rolling the
        // `2.5.29.35` extension.
        params.use_authority_key_identifier_extension = true;
        // A random, unique serial number per mint. rcgen's fallback (when
        // `serial_number` is left `None`) derives the serial from a SHA-256
        // digest of the certificate's *public key* alone - and since a
        // single leaf keypair is intentionally reused across every minted
        // host (see module docs), that fallback would hand out the exact
        // same serial number to every leaf this CA ever issues. Duplicate
        // serials from one issuer confuse certificate caches/path-building
        // in some verifiers, so mint a fresh 128-bit random value instead.
        params.serial_number = Some(SerialNumber::from_slice(Uuid::new_v4().as_bytes()));
        let now = OffsetDateTime::now_utc();
        params.not_before = now - Duration::days(1);
        params.not_after = now + Duration::days(397);

        let cert = params
            .signed_by(&self.leaf_key, &self.issuer, &self.ca_key)
            .map_err(cert_err)?;
        let cert_der = cert.der().clone();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.leaf_key.serialize_der()));
        Ok(MintedCert {
            cert_chain: vec![cert_der],
            key,
        })
    }

    /// Returns the CA certificate, PEM-encoded.
    pub fn pem(&self) -> String {
        self.ca_pem.clone()
    }

    /// Returns the CA certificate, DER-encoded.
    pub fn der(&self) -> Vec<u8> {
        self.ca_der.as_ref().to_vec()
    }

    /// Returns the SHA-256 fingerprint of the CA certificate, formatted as
    /// uppercase colon-separated hex (e.g. `AB:CD:12:...`).
    pub fn fingerprint_sha256(&self) -> String {
        let digest = Sha256::digest(self.ca_der.as_ref());
        digest
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":")
    }
}

/// Generates a brand-new root CA (self-signed, ECDSA P-256), returning its
/// certificate and private key, both PEM-encoded.
///
/// IMPORTANT: this function only runs from `load_or_generate` when no
/// `ca.pem`/`ca-key.pem` exist yet on disk. Any extensions added here apply
/// only to *newly minted* CAs; an already-installed CA (already trusted in
/// the user's OS/browser store) must never be regenerated or rewritten as a
/// side effect of a code change here, or that trust anchor would silently
/// break. Do not call this outside the "files absent" branch.
fn generate_ca_pem() -> Result<(String, String)> {
    let mut params = CertificateParams::new(Vec::<String>::new()).map_err(cert_err)?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "rdproxy CA");
    dn.push(DnType::OrganizationName, "rdproxy");
    dn.push(DnType::OrganizationalUnitName, "rdproxy Root CA");
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    // Self-referential Authority Key Identifier (AKI == this CA's own
    // Subject Key Identifier): standard practice for self-signed roots.
    // Because this is a self-signed cert, `self_signed` uses the CA's own
    // key/identifier method as the "issuer" when computing the AKI, so this
    // naturally comes out equal to the SKI written below by `IsCa::Ca`.
    params.use_authority_key_identifier_extension = true;
    let now = OffsetDateTime::now_utc();
    // Backdate not_before by a day to tolerate client clock skew.
    params.not_before = now - Duration::days(1);
    params.not_after = now + Duration::days(3650);

    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(cert_err)?;
    let cert = params.self_signed(&key).map_err(cert_err)?;
    Ok((cert.pem(), key.serialize_pem()))
}

/// Parses the first certificate out of a PEM document.
fn first_cert_der(pem: &str) -> Result<CertificateDer<'static>> {
    let mut reader = std::io::BufReader::new(pem.as_bytes());
    let mut certs = rustls_pemfile::certs(&mut reader);
    let first = certs.next();
    match first {
        Some(cert) => {
            cert.map_err(|e| ProxyError::Cert(format!("failed to parse CA certificate: {e}")))
        }
        None => Err(ProxyError::Cert(
            "CA pem file contained no certificate".to_string(),
        )),
    }
}

/// Writes `contents` to `path`, restricting permissions to owner-only
/// (`0600`) on unix so the CA private key is never world/group-readable.
fn write_secret_file(path: &Path, contents: &[u8]) -> Result<()> {
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_host_rejects_empty_and_oversized() {
        assert!(sanitize_host("").is_err());
        assert!(sanitize_host("   ").is_err());
        assert!(sanitize_host(&"a".repeat(300)).is_err());
        assert_eq!(sanitize_host("example.com").unwrap(), "example.com");
        assert_eq!(sanitize_host("[::1]").unwrap(), "::1");
        assert_eq!(sanitize_host("  example.com  ").unwrap(), "example.com");
    }

    #[test]
    fn restrict_alpn_keeps_only_known_protocols() {
        let offered = vec![b"h2".to_vec(), b"spdy/3".to_vec(), b"http/1.1".to_vec()];
        let restricted = restrict_alpn(&offered);
        assert_eq!(restricted, vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
    }

    #[test]
    fn host_cache_evicts_least_recently_used() {
        let mut cache = HostCache::new(2);
        let mint_for = |n: &str| -> Result<MintedCert> {
            let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(cert_err)?;
            let params = CertificateParams::new(vec![n.to_string()]).map_err(cert_err)?;
            let cert = params.self_signed(&key).map_err(cert_err)?;
            Ok(MintedCert {
                cert_chain: vec![cert.der().clone()],
                key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            })
        };
        cache.get_or_mint("a.com", || mint_for("a.com")).unwrap();
        cache.get_or_mint("b.com", || mint_for("b.com")).unwrap();
        // touch "a.com" so "b.com" becomes the LRU entry.
        cache.get_or_mint("a.com", || mint_for("a.com")).unwrap();
        cache.get_or_mint("c.com", || mint_for("c.com")).unwrap();
        assert!(cache.map.contains_key("a.com"));
        assert!(cache.map.contains_key("c.com"));
        assert!(!cache.map.contains_key("b.com"));
    }

    #[test]
    fn load_or_generate_is_stable_across_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let ca1 = CertAuthority::load_or_generate(dir.path()).unwrap();
        let der1 = ca1.der();
        let ca2 = CertAuthority::load_or_generate(dir.path()).unwrap();
        let der2 = ca2.der();
        assert_eq!(der1, der2);
    }

    #[test]
    fn minted_leaf_is_signed_by_ca_and_has_matching_san() {
        let dir = tempfile::tempdir().unwrap();
        let ca = CertAuthority::load_or_generate(dir.path()).unwrap();
        let minted = ca.mint("example.org").unwrap();
        assert_eq!(minted.cert_chain.len(), 1);
        let (_, cert) = x509_parser::parse_x509_certificate(minted.cert_chain[0].as_ref()).unwrap();
        let sans = cert
            .subject_alternative_name()
            .unwrap()
            .expect("SAN present");
        let matches_host = sans.value.general_names.iter().any(|gn| {
            matches!(gn, x509_parser::extensions::GeneralName::DNSName(n) if *n == "example.org")
        });
        assert!(matches_host);
    }

    /// Regression test for the strict-verifier rejection bug: a minted leaf
    /// must carry AKI, SKI, a critical Key Usage, and Basic Constraints
    /// (CA:FALSE), and the AKI must equal the CA's own SKI - otherwise
    /// OpenSSL-based clients (Python, Java, Go, Node, ...) reject the cert
    /// with "unable to get local issuer certificate" / "Missing Authority
    /// Key Identifier".
    #[test]
    fn minted_leaf_carries_strict_verifier_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let ca = CertAuthority::load_or_generate(dir.path()).unwrap();
        let minted = ca.mint("example.org").unwrap();
        let (_, leaf) = x509_parser::parse_x509_certificate(minted.cert_chain[0].as_ref()).unwrap();
        let ca_der = ca.der();
        let (_, ca_cert) = x509_parser::parse_x509_certificate(&ca_der).unwrap();

        // Basic Constraints: CA:FALSE, critical.
        let bc = leaf
            .basic_constraints()
            .unwrap()
            .expect("Basic Constraints present");
        assert!(bc.critical);
        assert!(!bc.value.ca);

        // Key Usage: digitalSignature + keyEncipherment, critical, and
        // nothing CA-ish like keyCertSign.
        let ku = leaf.key_usage().unwrap().expect("Key Usage present");
        assert!(ku.critical);
        assert!(ku.value.digital_signature());
        assert!(ku.value.key_encipherment());
        assert!(!ku.value.key_cert_sign());

        // Subject Key Identifier present on the leaf.
        let leaf_ski = leaf
            .iter_extensions()
            .find_map(|ext| match ext.parsed_extension() {
                x509_parser::extensions::ParsedExtension::SubjectKeyIdentifier(id) => {
                    Some(id.0.to_vec())
                }
                _ => None,
            })
            .expect("leaf Subject Key Identifier present");
        assert!(!leaf_ski.is_empty());

        // Authority Key Identifier present on the leaf, and equal to the
        // CA's own Subject Key Identifier.
        let leaf_aki = leaf
            .iter_extensions()
            .find_map(|ext| match ext.parsed_extension() {
                x509_parser::extensions::ParsedExtension::AuthorityKeyIdentifier(aki) => {
                    aki.key_identifier.as_ref().map(|k| k.0.to_vec())
                }
                _ => None,
            })
            .expect("leaf Authority Key Identifier present");
        let ca_ski = ca_cert
            .iter_extensions()
            .find_map(|ext| match ext.parsed_extension() {
                x509_parser::extensions::ParsedExtension::SubjectKeyIdentifier(id) => {
                    Some(id.0.to_vec())
                }
                _ => None,
            })
            .expect("CA Subject Key Identifier present");
        assert_eq!(leaf_aki, ca_ski, "leaf AKI must match the CA's own SKI");

        // Serial number: present and nonzero.
        assert!(
            !leaf.raw_serial().iter().all(|b| *b == 0),
            "serial must be nonzero"
        );
    }

    #[test]
    fn minted_leaf_serials_are_random_and_unique_per_mint() {
        let dir = tempfile::tempdir().unwrap();
        let ca = CertAuthority::load_or_generate(dir.path()).unwrap();
        // Call the uncached signing path directly (bypassing the host cache)
        // to prove each *mint operation* gets a fresh serial, not just each
        // distinct host: this is what protects against the shared-leaf-key
        // fallback-serial collision described in `mint_uncached`'s comments.
        let first = ca.mint_uncached("example.org").unwrap();
        let second = ca.mint_uncached("example.org").unwrap();
        let (_, first_cert) =
            x509_parser::parse_x509_certificate(first.cert_chain[0].as_ref()).unwrap();
        let (_, second_cert) =
            x509_parser::parse_x509_certificate(second.cert_chain[0].as_ref()).unwrap();
        assert_ne!(first_cert.raw_serial(), second_cert.raw_serial());
    }

    #[test]
    fn minted_leaf_for_ip_host_uses_ip_san_not_dns() {
        let dir = tempfile::tempdir().unwrap();
        let ca = CertAuthority::load_or_generate(dir.path()).unwrap();
        let minted = ca.mint("127.0.0.1").unwrap();
        let (_, cert) = x509_parser::parse_x509_certificate(minted.cert_chain[0].as_ref()).unwrap();
        let sans = cert
            .subject_alternative_name()
            .unwrap()
            .expect("SAN present");
        let has_ip_san = sans.value.general_names.iter().any(|gn| {
            matches!(gn, x509_parser::extensions::GeneralName::IPAddress(octets) if *octets == [127, 0, 0, 1])
        });
        assert!(has_ip_san, "IP host should mint an IP SAN");
        let has_dns_san = sans
            .value
            .general_names
            .iter()
            .any(|gn| matches!(gn, x509_parser::extensions::GeneralName::DNSName(_)));
        assert!(!has_dns_san, "IP host should not also carry a DNS SAN");
    }
}
