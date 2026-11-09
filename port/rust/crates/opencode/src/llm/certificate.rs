//! Certificate failures under the names Bun's fetch gives them (BoringSSL's X509 verification codes, recorded from
//! Bun 1.4.2): webpki reports any certificate it cannot chain to a root as `UnknownIssuer`, where BoringSSL tells a
//! self-signed leaf, a self-signed root sent in the chain, a lone leaf and an incomplete chain apart. [`Verifier`]
//! wraps the platform verifier to tell them apart from the chain the server sent; [`code`] names a TLS failure.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error, OtherError, SignatureScheme};

/// The code of a certificate webpki could not chain to a root, carried inside the rustls error.
#[derive(Debug)]
struct Code(&'static str);

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Code {}

fn certificate(code: &'static str) -> Error {
    Error::InvalidCertificate(CertificateError::Other(OtherError(Arc::new(Code(code)))))
}

#[derive(Debug)]
pub struct Verifier(pub Arc<dyn ServerCertVerifier>);

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        self.0.verify_server_cert(end_entity, intermediates, server_name, ocsp, now).map_err(|error| match error {
            // BoringSSL builds the chain first: an untrusted self-signed leaf is that, whatever else is wrong with it
            // (webpki calls one with CA basic constraints `CaUsedAsEndEntity`).
            _ if self_signed(end_entity) => certificate("DEPTH_ZERO_SELF_SIGNED_CERT"),
            Error::InvalidCertificate(CertificateError::UnknownIssuer) => {
                let code = if intermediates.iter().any(|cert| self_signed(cert)) {
                    "SELF_SIGNED_CERT_IN_CHAIN"
                } else if intermediates.is_empty() {
                    "UNABLE_TO_VERIFY_LEAF_SIGNATURE"
                } else {
                    "UNABLE_TO_GET_ISSUER_CERT_LOCALLY"
                };
                certificate(code)
            }
            other => other,
        })
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.0.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.0.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}

/// Bun's code for a TLS failure, when it names one: the certificate checks, and `UNKNOWN_CERTIFICATE_VERIFICATION_ERROR`
/// for a server that does not speak TLS.
pub fn code(error: &std::io::Error) -> Option<&'static str> {
    let rustls = error.get_ref()?.downcast_ref::<Error>()?;
    Some(match rustls {
        Error::InvalidCertificate(certificate) => match certificate {
            CertificateError::Other(OtherError(inner)) => inner.downcast_ref::<Code>()?.0,
            CertificateError::Expired | CertificateError::ExpiredContext { .. } => "CERT_HAS_EXPIRED",
            CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => "CERT_NOT_YET_VALID",
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => {
                "ERR_TLS_CERT_ALTNAME_INVALID"
            }
            CertificateError::BadSignature => "CERT_SIGNATURE_FAILURE",
            CertificateError::Revoked => "CERT_REVOKED",
            CertificateError::InvalidPurpose | CertificateError::InvalidPurposeContext { .. } => "INVALID_PURPOSE",
            _ => return None,
        },
        Error::InvalidMessage(_) => "UNKNOWN_CERTIFICATE_VERIFICATION_ERROR",
        _ => return None,
    })
}

/// BoringSSL's text for a verification code (`X509_verify_cert_error_string`); `None` where Bun words the failure as
/// `<code> fetching "<url>"`.
pub fn message(code: &str) -> Option<&'static str> {
    Some(match code {
        "DEPTH_ZERO_SELF_SIGNED_CERT" => "self signed certificate",
        "SELF_SIGNED_CERT_IN_CHAIN" => "self signed certificate in certificate chain",
        "UNABLE_TO_VERIFY_LEAF_SIGNATURE" => "unable to verify the first certificate",
        "UNABLE_TO_GET_ISSUER_CERT_LOCALLY" => "unable to get local issuer certificate",
        "CERT_HAS_EXPIRED" => "certificate has expired",
        "CERT_NOT_YET_VALID" => "certificate is not yet valid",
        "CERT_SIGNATURE_FAILURE" => "certificate signature failure",
        "CERT_REVOKED" => "certificate revoked",
        "INVALID_PURPOSE" => "unsupported certificate purpose",
        "UNKNOWN_CERTIFICATE_VERIFICATION_ERROR" => "unknown certificate verification error",
        _ => return None,
    })
}

/// Whether a certificate names itself as its issuer (its DER `issuer` and `subject` Names are equal).
fn self_signed(cert: &CertificateDer<'_>) -> bool {
    names(cert.as_ref()).is_some_and(|(issuer, subject)| issuer == subject)
}

/// The issuer and subject of an X.509 certificate: `Certificate ::= SEQUENCE { tbsCertificate SEQUENCE { [0] version
/// OPTIONAL, serialNumber, signature, issuer, validity, subject, ... }, ... }`.
fn names(der: &[u8]) -> Option<(&[u8], &[u8])> {
    let (_, certificate, _) = element(der)?;
    let (_, mut tbs, _) = element(certificate)?;
    let mut fields = Vec::with_capacity(6);
    while fields.len() < 6 && !tbs.is_empty() {
        let (tag, _, rest) = element(tbs)?;
        let whole = &tbs[..tbs.len() - rest.len()];
        tbs = rest;
        // The explicit version is optional and context-tagged.
        if fields.is_empty() && tag == 0xa0 {
            continue;
        }
        fields.push(whole);
    }
    // serialNumber, signature, issuer, validity, subject.
    Some((fields.get(2)?, fields.get(4)?))
}

/// One DER element: its tag, contents and what follows it.
fn element(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, mut rest) = rest.split_first()?;
    let length = if first < 0x80 {
        first as usize
    } else {
        let count = (first & 0x7f) as usize;
        if count == 0 || count > 4 || rest.len() < count {
            return None;
        }
        let length = rest[..count].iter().fold(0usize, |n, b| (n << 8) | *b as usize);
        rest = &rest[count..];
        length
    };
    (rest.len() >= length).then(|| (tag, &rest[..length], &rest[length..]))
}
