//! Digital signatures: signing a document with a certificate, and checking the
//! signatures a document carries.
//!
//! A PDF signature is a CMS (PKCS #7) structure stored in the file, computed
//! over every byte of the file except the place the structure itself occupies.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use clap::Args;
use cms::builder::{SignedDataBuilder, SignerInfoBuilder};
use cms::cert::{CertificateChoices, IssuerAndSerialNumber};
use cms::content_info::ContentInfo;
use cms::signed_data::{EncapsulatedContentInfo, SignedData, SignerIdentifier, SignerInfo};
use der::asn1::ObjectIdentifier;
use der::{Decode, Encode};
use lopdf::{Dictionary, Document, Object, Stream, StringFormat};
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};
use rsa::signature::Verifier;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256, Sha384, Sha512};
use x509_cert::Certificate;
use x509_cert::spki::AlgorithmIdentifierOwned;

use crate::doc;
use crate::font::TextFont;
use crate::ops::edit::visual_space;
use crate::ops::redact::{apply, bounds, parse_rect};

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct SignArgs {
    /// PDF file to sign
    pub input: PathBuf,
    /// Where to write the signed PDF (may be the input file)
    #[arg(short, long)]
    pub output: PathBuf,
    /// Certificate in PEM form; further certificates in the file are included as the chain
    #[arg(long)]
    pub cert: Option<PathBuf>,
    /// Private key in PEM form (PKCS #8 or RSA), matching the certificate
    #[arg(long)]
    pub key: Option<PathBuf>,
    /// PKCS #12 file (.p12 or .pfx) holding both key and certificates, instead of cert and key
    #[arg(long)]
    pub p12: Option<PathBuf>,
    /// Password of the PKCS #12 file (default: empty)
    #[arg(long)]
    pub p12_password: Option<String>,
    /// Why the document is signed, recorded in the signature
    #[arg(long)]
    pub reason: Option<String>,
    /// Where it is signed, recorded in the signature
    #[arg(long)]
    pub location: Option<String>,
    /// Signer's name (default: the certificate's common name)
    #[arg(long)]
    pub name: Option<String>,
    /// Show the signature on a page, as a box with the signer's name, the date and the reason: "page:x0,y0,x1,y1" in the coordinates layout reports (default: the signature is not shown)
    #[arg(long)]
    pub visible: Option<String>,
    /// Address of a timestamp authority (RFC 3161), e.g. "http://timestamp.digicert.com"; its signed statement of the time is embedded, so the signature can be shown to have existed then
    #[arg(long)]
    pub tsa: Option<String>,
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct SignaturesArgs {
    /// PDF file to check
    pub input: PathBuf,
    /// PEM file of certificates you trust, such as your organisation's root; each signer's chain is checked against them
    #[arg(long)]
    pub trust: Option<PathBuf>,
    /// Also download the revocation lists that the certificates of a trusted chain name, and check that none of them was revoked (needs network access)
    #[arg(long)]
    #[serde(default)]
    pub revocation: bool,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

const SHA1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.14.3.2.26");
const SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.1");
const SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.2");
const SHA512: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.3");
const DATA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.1");
const SIGNED_DATA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.2");
const MESSAGE_DIGEST: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.4");
const SIGNING_TIME: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.5");
const RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");
const EC: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
const COMMON_NAME: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.3");
const RSA_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.11");
const RSA_SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.12");
const RSA_SHA512: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.13");
const ECDSA_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2");
const BASIC_CONSTRAINTS: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.19");
const CRL_DISTRIBUTION_POINTS: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.31");
const TIMESTAMP_TOKEN: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.2.14");

/// Room reserved in the file for the signature; the real one is padded to fit.
const HOLE: usize = 16384;

/// Room for the signature, with more where an authority's timestamp joins it.
fn hole(a: &SignArgs) -> usize {
    if a.tsa.is_some() { 2 * HOLE } else { HOLE }
}

/// A timestamp authority's token: when it says it was made, and the digest it is about.
struct Timestamp {
    /// Seconds since 1970.
    time: u64,
    about: Vec<u8>,
}

/// Reads a timestamp token. The authority's own signature on it is not checked.
fn read_timestamp(token: &der::Any) -> Option<Timestamp> {
    let info = token.decode_as::<ContentInfo>().ok()?;
    let data = info.content.decode_as::<SignedData>().ok()?;
    let statement = data.encap_content_info.econtent?;
    // TSTInfo: version, policy, the digest it is about, serial number, time, ...
    let fields = Vec::<der::Any>::from_der(statement.value()).ok()?;
    let imprint = fields.get(2)?.decode_as::<Vec<der::Any>>().ok()?;
    let time = fields
        .get(4)?
        .decode_as::<der::asn1::GeneralizedTime>()
        .ok()?;
    Some(Timestamp {
        time: time.to_unix_duration().as_secs(),
        about: imprint.get(1)?.value().to_vec(),
    })
}

/// Asks a timestamp authority to state when `signature` existed. Returns its token.
fn request_timestamp(url: &str, signature: &[u8]) -> Result<der::Any> {
    let digest = Sha256::digest(signature);
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).map_err(|e| anyhow!("no random numbers: {e}"))?;
    // A positive number of full length.
    nonce[0] = nonce[0] & 0x7f | 0x40;
    // TimeStampReq: version 1, SHA-256 digest, a nonce, and "send your certificate".
    let mut request = vec![0x30, 0x43, 0x02, 0x01, 0x01, 0x30, 0x31, 0x30, 0x0d];
    request.extend_from_slice(&[
        0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01,
    ]);
    request.extend_from_slice(&[0x05, 0x00, 0x04, 0x20]);
    request.extend_from_slice(&digest);
    request.extend_from_slice(&[0x02, 0x08]);
    request.extend_from_slice(&nonce);
    request.extend_from_slice(&[0x01, 0x01, 0xff]);

    let failed = |why: String| anyhow!("no timestamp from {url}: {why}");
    let mut response = ureq::post(url)
        .header("Content-Type", "application/timestamp-query")
        .send(&request[..])
        .map_err(|e| failed(e.to_string()))?;
    let reply = response
        .body_mut()
        .with_config()
        .limit(1024 * 1024)
        .read_to_vec()
        .map_err(|e| failed(e.to_string()))?;
    // TimeStampResp: a status, and the token when the status is "granted".
    let parts = Vec::<der::Any>::from_der(&reply)
        .map_err(|_| failed("the reply is not a timestamp response".into()))?;
    let granted = parts
        .first()
        .and_then(|status| status.decode_as::<Vec<der::Any>>().ok())
        .and_then(|status| status.first().map(|code| code.value().to_vec()))
        .is_some_and(|code| code == [0] || code == [1]);
    let token = parts
        .get(1)
        .filter(|_| granted)
        .ok_or_else(|| failed("the request was refused".into()))?;
    let stated = read_timestamp(token).ok_or_else(|| failed("the token cannot be read".into()))?;
    if stated.about != digest[..] {
        return Err(failed(
            "the token is about something else than this signature".into(),
        ));
    }
    Ok(token.clone())
}
/// What the byte range looks like before the real offsets are known. Each
/// number is as wide as an offset can get, so the final text fits in its place.
const RANGE_PLACEHOLDER: i64 = 9_999_999_999;

/// The private key, in one of the forms that can sign.
enum Key {
    Rsa(Box<rsa::RsaPrivateKey>),
    P256(p256::ecdsa::SigningKey),
}

struct Identity {
    key: Key,
    /// The signer's certificate first, then the rest of its chain.
    certs: Vec<Certificate>,
}

fn parse_key(pem_or_der: &[u8]) -> Result<Key> {
    let unsupported =
        || anyhow!("the private key is not an RSA or P-256 key in PKCS #8 or PKCS #1 form");
    if let Ok(text) = std::str::from_utf8(pem_or_der) {
        if text.contains("ENCRYPTED PRIVATE KEY") {
            bail!(
                "the private key is encrypted; decrypt it first or use a PKCS #12 file with its password"
            );
        }
        if text.contains("BEGIN RSA PRIVATE KEY") {
            return Ok(Key::Rsa(Box::new(
                rsa::RsaPrivateKey::from_pkcs1_pem(text).map_err(|_| unsupported())?,
            )));
        }
        if text.contains("BEGIN PRIVATE KEY") {
            if let Ok(key) = rsa::RsaPrivateKey::from_pkcs8_pem(text) {
                return Ok(Key::Rsa(Box::new(key)));
            }
            return p256::ecdsa::SigningKey::from_pkcs8_pem(text)
                .map(Key::P256)
                .map_err(|_| unsupported());
        }
    }
    if let Ok(key) = rsa::RsaPrivateKey::from_pkcs8_der(pem_or_der) {
        return Ok(Key::Rsa(Box::new(key)));
    }
    p256::ecdsa::SigningKey::from_pkcs8_der(pem_or_der)
        .map(Key::P256)
        .map_err(|_| unsupported())
}

impl Key {
    /// The public half as a SubjectPublicKeyInfo, to find the matching certificate.
    fn public_der(&self) -> Result<Vec<u8>> {
        use rsa::pkcs8::EncodePublicKey;
        Ok(match self {
            Key::Rsa(key) => key.to_public_key().to_public_key_der()?.into_vec(),
            Key::P256(key) => key.verifying_key().to_public_key_der()?.into_vec(),
        })
    }
}

fn load_identity(a: &SignArgs) -> Result<Identity> {
    let read = |path: &Path| {
        std::fs::read(path).map_err(|e| anyhow!("cannot read {}: {e}", path.display()))
    };
    let (key, mut certs) = match (&a.p12, &a.cert, &a.key) {
        (Some(p12), None, None) => {
            let store = p12_keystore::KeyStore::from_pkcs12(
                &read(p12)?,
                a.p12_password.as_deref().unwrap_or(""),
                p12_keystore::Pkcs12ImportPolicy::Relaxed,
            )
            .map_err(|e| anyhow!("cannot open {}: {e} (wrong password?)", p12.display()))?;
            let (_, chain) = store
                .private_key_chain()
                .ok_or_else(|| anyhow!("{} holds no private key", p12.display()))?;
            let certs = chain
                .certs()
                .iter()
                .map(|c| {
                    Certificate::from_der(c.as_der())
                        .map_err(|e| anyhow!("unreadable certificate: {e}"))
                })
                .collect::<Result<Vec<_>>>()?;
            (parse_key(chain.key().as_der())?, certs)
        }
        (None, Some(cert), Some(key)) => {
            let certs = Certificate::load_pem_chain(&read(cert)?)
                .map_err(|e| anyhow!("{} is not a PEM certificate: {e}", cert.display()))?;
            (parse_key(&read(key)?)?, certs)
        }
        _ => bail!("give either a PKCS #12 file, or a certificate together with its key"),
    };
    // The signer's certificate is the one that carries this key's public half.
    let public = key.public_der()?;
    let own = certs
        .iter()
        .position(|c| {
            c.tbs_certificate
                .subject_public_key_info
                .to_der()
                .is_ok_and(|der| der == public)
        })
        .ok_or_else(|| anyhow!("none of the certificates belongs to the private key"))?;
    certs.swap(0, own);
    Ok(Identity { key, certs })
}

fn common_name(name: &x509_cert::name::Name) -> Option<String> {
    name.0
        .iter()
        .flat_map(|rdn| rdn.0.iter())
        .find(|atv| atv.oid == COMMON_NAME)
        .and_then(|atv| {
            let bytes = atv.value.value();
            // Common names are UTF-8, printable or BMP strings; the first two read as text directly.
            std::str::from_utf8(bytes).ok().map(str::to_string)
        })
}

/// Builds the detached CMS structure over a digest of the document.
fn cms_signature(identity: &Identity, digest: &[u8], tsa: Option<&str>) -> Result<Vec<u8>> {
    let content = EncapsulatedContentInfo {
        econtent_type: DATA,
        econtent: None,
    };
    let algorithm = AlgorithmIdentifierOwned {
        oid: SHA256,
        parameters: None,
    };
    let own = &identity.certs[0];
    let signer_id = SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
        issuer: own.tbs_certificate.issuer.clone(),
        serial_number: own.tbs_certificate.serial_number.clone(),
    });
    let mut builder = SignedDataBuilder::new(&content);
    builder
        .add_digest_algorithm(algorithm.clone())
        .map_err(|e| anyhow!("cannot build the signature: {e}"))?;
    for cert in &identity.certs {
        builder
            .add_certificate(CertificateChoices::Certificate(cert.clone()))
            .map_err(|e| anyhow!("cannot build the signature: {e}"))?;
    }
    let failed = |e: cms::builder::Error| anyhow!("cannot build the signature: {e}");
    let signed = match &identity.key {
        Key::Rsa(key) => {
            let signer = rsa::pkcs1v15::SigningKey::<Sha256>::new((**key).clone());
            let info =
                SignerInfoBuilder::new(&signer, signer_id, algorithm, &content, Some(digest))
                    .map_err(failed)?;
            builder
                .add_signer_info::<_, rsa::pkcs1v15::Signature>(info)
                .map_err(failed)?
                .build()
                .map_err(failed)?
        }
        Key::P256(key) => {
            let info = SignerInfoBuilder::new(key, signer_id, algorithm, &content, Some(digest))
                .map_err(failed)?;
            builder
                .add_signer_info::<_, p256::ecdsa::DerSignature>(info)
                .map_err(failed)?
                .build()
                .map_err(failed)?
        }
    };
    let Some(tsa) = tsa else {
        return Ok(signed.to_der()?);
    };
    // The authority's statement is about the signature value, and joins the signer's
    // entry as an attribute that is not itself signed.
    let mut data = signed.content.decode_as::<SignedData>()?;
    let mut signers = data.signer_infos.0.into_vec();
    let signer = signers
        .first_mut()
        .ok_or_else(|| anyhow!("cannot build the signature: no signer"))?;
    let token = request_timestamp(tsa, signer.signature.as_bytes())?;
    let mut values = der::asn1::SetOfVec::new();
    values.insert(token)?;
    let mut attributes = der::asn1::SetOfVec::new();
    attributes.insert(x509_cert::attr::Attribute {
        oid: TIMESTAMP_TOKEN,
        values,
    })?;
    signer.unsigned_attrs = Some(attributes);
    data.signer_infos = signers.try_into()?;
    Ok(ContentInfo {
        content_type: SIGNED_DATA,
        content: der::Any::encode_from(&data)?,
    }
    .to_der()?)
}

/// Days since 1970-01-01 to a calendar date (proleptic Gregorian).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

/// Seconds since the Unix epoch as a date and time a person reads, in UTC.
fn readable_date(seconds: u64) -> String {
    let (year, month, day) = civil((seconds / 86_400) as i64);
    let rest = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rest / 3600,
        rest % 3600 / 60
    )
}

/// What a signature that is shown looks like: a frame with lines of text in it, as
/// large as fits the box, up to ten points.
fn appearance(
    d: &mut Document,
    width: f64,
    height: f64,
    lines: &[String],
) -> Result<lopdf::ObjectId> {
    let font = TextFont::new(d, &lines.join("\n"), None)?;
    let widest = lines
        .iter()
        .map(|line| font.width(line, 1.0))
        .fold(0.001, f64::max);
    let size = ((height - 6.0) / (lines.len() as f64 * 1.2))
        .min((width - 8.0) / widest)
        .clamp(1.0, 10.0);
    let names: Vec<String> = (0..font.ids().len()).map(|k| format!("F{k}")).collect();
    let mut fonts = Dictionary::new();
    for (name, id) in names.iter().zip(font.ids()) {
        fonts.set(name.as_str(), id);
    }
    let mut content = format!(
        "q\n0.5 w\n0.45 G\n0.25 0.25 {:.2} {:.2} re\nS\nQ\nBT\n0 g\n",
        width - 0.5,
        height - 0.5
    );
    for (i, line) in lines.iter().enumerate() {
        content += &format!(
            "1 0 0 1 4 {:.2} Tm\n{}\n",
            height - 3.0 - size * (0.9 + 1.2 * i as f64),
            font.show_named(&names, line, size)
        );
    }
    content += "ET\n";
    let mut resources = Dictionary::new();
    resources.set("Font", fonts);
    let mut form = Dictionary::new();
    form.set("Type", Object::Name(b"XObject".to_vec()));
    form.set("Subtype", Object::Name(b"Form".to_vec()));
    form.set(
        "BBox",
        vec![
            0.into(),
            0.into(),
            Object::Real(width as f32),
            Object::Real(height as f32),
        ],
    );
    form.set("Resources", resources);
    Ok(d.add_object(Stream::new(form, content.into_bytes())))
}

/// Seconds since the Unix epoch as a PDF date, in UTC.
fn pdf_date(seconds: u64) -> String {
    let (year, month, day) = civil((seconds / 86_400) as i64);
    let rest = seconds % 86_400;
    format!(
        "D:{year:04}{month:02}{day:02}{:02}{:02}{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Adds the signature dictionary and its invisible form field to `target`.
///
/// `source` is where the existing page and form are read from; the changed copies
/// go into `target`. For a full rewrite the two are the same document; for an
/// incremental update `target` holds only what is appended.
fn add_signature(
    source: &Document,
    target: &mut Document,
    signer: Option<&str>,
    a: &SignArgs,
    now: u64,
) -> Result<()> {
    let wide = Object::Integer(RANGE_PLACEHOLDER);
    let mut sig = Dictionary::new();
    sig.set("Type", Object::Name(b"Sig".to_vec()));
    sig.set("Filter", Object::Name(b"Adobe.PPKLite".to_vec()));
    sig.set("SubFilter", Object::Name(b"adbe.pkcs7.detached".to_vec()));
    sig.set(
        "ByteRange",
        vec![Object::Integer(0), wide.clone(), wide.clone(), wide],
    );
    sig.set(
        "Contents",
        Object::String(vec![0; hole(a)], StringFormat::Hexadecimal),
    );
    sig.set("M", Object::string_literal(pdf_date(now)));
    for (key, value) in [
        ("Name", signer),
        ("Reason", a.reason.as_deref()),
        ("Location", a.location.as_deref()),
    ] {
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            sig.set(key, lopdf::text_string(value));
        }
    }
    let sig = target.add_object(sig);

    let pages = doc::page_ids(source);
    let shown = a
        .visible
        .as_deref()
        .map(|spec| parse_rect(spec, pages.len() as u32))
        .transpose()?;
    let page = *pages
        .get(shown.map_or(0, |(n, _)| n as usize - 1))
        .ok_or_else(|| anyhow!("the document has no pages"))?;
    let catalog_id = doc::catalog_id(source)?;
    let catalog = source.get_dictionary(catalog_id)?;
    let form_entry = catalog.get(b"AcroForm").ok().cloned();
    let mut form = form_entry
        .as_ref()
        .and_then(|f| doc::resolve(source, f).as_dict().ok())
        .cloned()
        .unwrap_or_default();
    let mut fields: Vec<Object> = form
        .get(b"Fields")
        .ok()
        .and_then(|f| doc::resolve(source, f).as_array().ok())
        .cloned()
        .unwrap_or_default();

    // The signature lives in a form field: on the first page and without extent, or
    // where it was asked to be shown.
    let mut field = Dictionary::new();
    field.set("Type", Object::Name(b"Annot".to_vec()));
    field.set("Subtype", Object::Name(b"Widget".to_vec()));
    field.set("FT", Object::Name(b"Sig".to_vec()));
    field.set(
        "T",
        Object::string_literal(format!("Signature{}", fields.len() + 1)),
    );
    field.set("V", sig);
    field.set("P", page);
    match shown {
        None => field.set("Rect", vec![Object::Integer(0); 4]),
        Some((_, area)) => {
            // Layout coordinates have their origin at the top-left; the field's are the page's own.
            let (to_user, _, height) =
                visual_space(doc::page_box(source, page), doc::rotation(source, page));
            let rect = bounds(
                [(area[0], area[1]), (area[2], area[3])]
                    .map(|(x, y)| apply(to_user, x, height - y)),
            );
            let mut lines = vec![
                format!(
                    "Digitally signed by {}",
                    signer.unwrap_or("the holder of the key")
                ),
                readable_date(now),
            ];
            lines.extend(
                [a.reason.as_deref(), a.location.as_deref()]
                    .into_iter()
                    .flatten()
                    .filter(|line| !line.is_empty())
                    .map(str::to_string),
            );
            let look = appearance(target, rect[2] - rect[0], rect[3] - rect[1], &lines)?;
            field.set("Rect", rect.map(|v| Object::Real(v as f32)).to_vec());
            let mut states = Dictionary::new();
            states.set("N", look);
            field.set("AP", states);
        }
    }
    // Printed, and locked against changes.
    field.set("F", 132);
    let field = target.add_object(field);

    fields.push(Object::Reference(field));
    form.set("Fields", fields);
    // Bit 1: signatures exist. Bit 2: the file must only be changed by appending.
    form.set("SigFlags", 3);
    match form_entry {
        Some(Object::Reference(id)) => target.set_object(id, form),
        _ => {
            let mut catalog = catalog.clone();
            catalog.set("AcroForm", form);
            target.set_object(catalog_id, catalog);
        }
    }

    let mut page_dict = source.get_dictionary(page)?.clone();
    let mut annots = page_dict
        .get(b"Annots")
        .ok()
        .and_then(|o| doc::resolve(source, o).as_array().ok())
        .cloned()
        .unwrap_or_default();
    annots.push(Object::Reference(field));
    page_dict.set("Annots", annots);
    target.set_object(page, page_dict);
    Ok(())
}

pub fn sign(a: SignArgs) -> Result<Value> {
    let identity = load_identity(&a)?;
    // Checks that the file is sound; the bytes are needed again for appending.
    let (mut d, rebuilt) = doc::load_noting(&a.input, None)?;
    if d.was_encrypted() {
        bail!(
            "{} is encrypted; remove the protection first with the decrypt command",
            a.input.display()
        );
    }
    let earlier = signature_dicts(&d).len();
    let signer = a
        .name
        .clone()
        .or_else(|| common_name(&identity.certs[0].tbs_certificate.subject));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |t| t.as_secs());

    // Appending leaves every existing byte in place, which is what keeps earlier
    // signatures valid. Rewriting the file is the fallback for the few layouts
    // that cannot be appended to.
    let original =
        std::fs::read(&a.input).map_err(|e| anyhow!("cannot read {}: {e}", a.input.display()))?;
    let appended = (|| -> Result<Vec<u8>> {
        // A rebuilt document no longer matches the bytes it came from.
        if rebuilt {
            bail!("the file was damaged and had to be rebuilt");
        }
        let mut update = lopdf::IncrementalDocument::create_from(original.clone(), d.clone());
        let mut addition = std::mem::take(&mut update.new_document);
        add_signature(&d, &mut addition, signer.as_deref(), &a, now)?;
        update.new_document = addition;
        let mut bytes = Vec::new();
        update.save_to(&mut bytes)?;
        // The appended section must point back at the one before it. Where the original
        // cross-reference table was itself unreadable, it cannot, and readers would see
        // only the appended objects.
        if find(&bytes[original.len()..], b"/Prev").is_none() {
            bail!("the original cross-reference section cannot be chained to");
        }
        Ok(bytes)
    })();
    let (mut bytes, searched_from, kept) = match appended {
        Ok(bytes) => (bytes, original.len(), true),
        Err(_) => {
            let source = d.clone();
            add_signature(&source, &mut d, signer.as_deref(), &a, now)?;
            doc::seal(&mut d)?;
            let mut bytes = Vec::new();
            d.save_to(&mut bytes)?;
            (bytes, 0, false)
        }
    };

    // Now that the file exists, find the hole and say which bytes are signed.
    let room = hole(&a);
    let hole = format!("/Contents<{}>", "0".repeat(room * 2));
    let at = searched_from
        + find(&bytes[searched_from..], hole.as_bytes())
            .ok_or_else(|| anyhow!("cannot locate the signature in the written file"))?;
    let (start, end) = (at + "/Contents".len(), at + hole.len());
    let placeholder =
        format!("/ByteRange[0 {RANGE_PLACEHOLDER} {RANGE_PLACEHOLDER} {RANGE_PLACEHOLDER}]");
    let range_at = searched_from
        + find(&bytes[searched_from..], placeholder.as_bytes())
            .ok_or_else(|| anyhow!("cannot locate the byte range in the written file"))?;
    let range = format!("/ByteRange[0 {start} {end} {}]", bytes.len() - end);
    let padded = format!("{range:<width$}", width = placeholder.len());
    bytes[range_at..range_at + placeholder.len()].copy_from_slice(padded.as_bytes());

    let mut hasher = Sha256::new();
    hasher.update(&bytes[..start]);
    hasher.update(&bytes[end..]);
    let cms = cms_signature(&identity, &hasher.finalize(), a.tsa.as_deref())?;
    if cms.len() > room {
        bail!(
            "the certificate chain is too large to embed ({} bytes)",
            cms.len()
        );
    }
    let hex: String = cms.iter().map(|b| format!("{b:02X}")).collect();
    bytes[start + 1..start + 1 + hex.len()].copy_from_slice(hex.as_bytes());

    doc::write_atomic(&a.output, &bytes)?;
    Ok(json!({
        "output": a.output,
        "signer": signer,
        "algorithm": match identity.key { Key::Rsa(_) => "RSA with SHA-256", Key::P256(_) => "ECDSA P-256 with SHA-256" },
        "certificates_embedded": identity.certs.len(),
        "earlier_signatures": earlier,
        "earlier_signatures_kept": kept || earlier == 0,
        "timestamped": a.tsa.is_some(),
        "size_bytes": bytes.len(),
    }))
}

/// Signature dictionaries of the document, with the name of the field holding each.
fn signature_dicts(d: &Document) -> Vec<(String, &Dictionary)> {
    let mut found = Vec::new();
    for object in d.objects.values() {
        let Ok(field) = object.as_dict() else {
            continue;
        };
        if field.get(b"FT").ok().and_then(|t| t.as_name().ok()) != Some(b"Sig") {
            continue;
        }
        let value = field
            .get(b"V")
            .ok()
            .map(|v| doc::resolve(d, v))
            .and_then(|v| v.as_dict().ok());
        if let Some(sig) = value.filter(|s| s.has(b"ByteRange") && s.has(b"Contents")) {
            let name = field
                .get(b"T")
                .ok()
                .and_then(|t| doc::text(doc::resolve(d, t)))
                .unwrap_or_default();
            found.push((name, sig));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// The length of the DER element starting at `bytes[0]`, header included.
fn der_length(bytes: &[u8]) -> Option<usize> {
    let first = *bytes.get(1)? as usize;
    if first < 0x80 {
        return Some(2 + first);
    }
    let count = first & 0x7f;
    let length = bytes
        .get(2..2 + count)?
        .iter()
        .fold(0usize, |n, b| (n << 8) | *b as usize);
    Some(2 + count + length)
}

/// Checks one signature against the file. Returns what was established.
/// Signatures as the raw file states them: each byte range with the CMS structure in its gap.
///
/// Read from the bytes rather than from the parsed document, so that a file
/// someone has tampered with can still be judged, however broken it now is.
fn raw_signatures(file: &[u8]) -> Vec<([usize; 4], Vec<u8>)> {
    let pattern = regex::bytes::Regex::new(r"/ByteRange\s*\[\s*(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s*\]")
        .expect("a valid pattern");
    let mut found = Vec::new();
    for captures in pattern.captures_iter(file) {
        let number = |i: usize| {
            std::str::from_utf8(&captures[i])
                .ok()
                .and_then(|n| n.parse::<usize>().ok())
        };
        let (Some(a), Some(b), Some(c), Some(len)) = (number(1), number(2), number(3), number(4))
        else {
            continue;
        };
        // The gap between the two signed stretches holds the signature as a hex string.
        let gap = a
            .checked_add(b)
            .and_then(|start| file.get(start..c))
            .unwrap_or_default();
        let digits: Vec<u8> = gap.iter().copied().filter(u8::is_ascii_hexdigit).collect();
        let contents = digits
            .as_chunks::<2>()
            .0
            .iter()
            .filter_map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
            .collect();
        found.push(([a, b, c, len], contents));
    }
    found
}

/// Whether the key of `issuer` made the signature on `cert`. `None` where the
/// algorithm is not one that is checked.
fn issued_by(cert: &Certificate, issuer: &Certificate) -> Option<bool> {
    if cert.tbs_certificate.issuer != issuer.tbs_certificate.subject {
        return Some(false);
    }
    made_by(
        &cert.tbs_certificate.to_der().ok()?,
        cert.signature.raw_bytes(),
        cert.signature_algorithm.oid,
        issuer,
    )
}

/// Whether the key of `issuer` made `signature` over `message`. `None` where the
/// algorithm is not one that is checked.
fn made_by(
    message: &[u8],
    signature: &[u8],
    algorithm: ObjectIdentifier,
    issuer: &Certificate,
) -> Option<bool> {
    let spki = issuer
        .tbs_certificate
        .subject_public_key_info
        .to_der()
        .ok()?;
    let rsa_key = || rsa::RsaPublicKey::from_public_key_der(&spki).ok();
    let rsa_signature = || rsa::pkcs1v15::Signature::try_from(signature).ok();
    match algorithm {
        oid if oid == RSA_SHA256 => Some(
            rsa::pkcs1v15::VerifyingKey::<Sha256>::new(rsa_key()?)
                .verify(message, &rsa_signature()?)
                .is_ok(),
        ),
        oid if oid == RSA_SHA384 => Some(
            rsa::pkcs1v15::VerifyingKey::<Sha384>::new(rsa_key()?)
                .verify(message, &rsa_signature()?)
                .is_ok(),
        ),
        oid if oid == RSA_SHA512 => Some(
            rsa::pkcs1v15::VerifyingKey::<Sha512>::new(rsa_key()?)
                .verify(message, &rsa_signature()?)
                .is_ok(),
        ),
        oid if oid == ECDSA_SHA256 => {
            let key = p256::ecdsa::VerifyingKey::from_public_key_der(&spki).ok()?;
            let signature = p256::ecdsa::DerSignature::from_bytes(signature).ok()?;
            Some(key.verify(message, &signature).is_ok())
        }
        _ => None,
    }
}

/// Where a certificate says the list of its issuer's revoked certificates is kept.
fn revocation_list_address(cert: &Certificate) -> Option<String> {
    use x509_cert::ext::pkix::crl::dp::DistributionPoint;
    use x509_cert::ext::pkix::name::{DistributionPointName, GeneralName};
    let extension = cert
        .tbs_certificate
        .extensions
        .iter()
        .flatten()
        .find(|extension| extension.extn_id == CRL_DISTRIBUTION_POINTS)?;
    let points = Vec::<DistributionPoint>::from_der(extension.extn_value.as_bytes()).ok()?;
    points
        .into_iter()
        .filter_map(|point| match point.distribution_point? {
            DistributionPointName::FullName(names) => Some(names),
            _ => None,
        })
        .flatten()
        .find_map(|name| match name {
            GeneralName::UniformResourceIdentifier(address)
                if address.as_str().starts_with("http") =>
            {
                Some(address.as_str().to_string())
            }
            _ => None,
        })
}

/// Checks each certificate of a chain against the revocation list its issuer keeps.
/// `Ok` when every list was read and names none of them.
fn check_revocation(chain: &[(&Certificate, &Certificate)]) -> Result<(), String> {
    let name = |cert: &Certificate| {
        common_name(&cert.tbs_certificate.subject)
            .unwrap_or_else(|| cert.tbs_certificate.subject.to_string())
    };
    for (cert, issuer) in chain {
        let unknown = |why: String| format!("not established for {}: {why}", name(cert));
        let address = revocation_list_address(cert)
            .ok_or_else(|| unknown("its certificate names no revocation list".into()))?;
        let bytes = ureq::get(&address)
            .call()
            .map_err(|e| e.to_string())
            .and_then(|mut response| {
                response
                    .body_mut()
                    .with_config()
                    .limit(64 * 1024 * 1024)
                    .read_to_vec()
                    .map_err(|e| e.to_string())
            })
            .map_err(|e| unknown(format!("cannot download {address}: {e}")))?;
        let list = x509_cert::crl::CertificateList::from_der(&bytes)
            .map_err(|_| unknown(format!("{address} is not a revocation list")))?;
        let genuine = list.tbs_cert_list.to_der().ok().and_then(|message| {
            made_by(
                &message,
                list.signature.raw_bytes(),
                list.signature_algorithm.oid,
                issuer,
            )
        });
        if genuine != Some(true) {
            return Err(unknown(format!(
                "the list at {address} is not signed by the issuer"
            )));
        }
        let revoked = list
            .tbs_cert_list
            .revoked_certificates
            .iter()
            .flatten()
            .any(|entry| entry.serial_number == cert.tbs_certificate.serial_number);
        if revoked {
            return Err(format!("the certificate of {} was revoked", name(cert)));
        }
    }
    Ok(())
}

/// Whether a certificate says of itself that it may issue others.
fn may_issue(cert: &Certificate) -> bool {
    cert.tbs_certificate
        .extensions
        .iter()
        .flatten()
        .find(|extension| extension.extn_id == BASIC_CONSTRAINTS)
        .and_then(|extension| {
            x509_cert::ext::pkix::BasicConstraints::from_der(extension.extn_value.as_bytes()).ok()
        })
        .is_some_and(|constraints| constraints.ca)
}

/// Follows the signer's certificate up through the embedded ones to a trusted one.
///
/// Every certificate on the way must have been in date at `at`, seconds since 1970,
/// and every one that issued another must be allowed to. Returns the steps taken:
/// each certificate with the one that issued it.
fn chain_of_trust<'a>(
    signer: &'a Certificate,
    embedded: &[&'a Certificate],
    trusted: &'a [Certificate],
    at: u64,
) -> Result<Vec<(&'a Certificate, &'a Certificate)>, String> {
    let name = |cert: &Certificate| {
        common_name(&cert.tbs_certificate.subject)
            .unwrap_or_else(|| cert.tbs_certificate.subject.to_string())
    };
    let in_date = |cert: &Certificate| {
        let validity = &cert.tbs_certificate.validity;
        (validity.not_before.to_unix_duration().as_secs()
            ..=validity.not_after.to_unix_duration().as_secs())
            .contains(&at)
    };
    let mut current = signer;
    let mut steps = Vec::new();
    for _ in 0..12 {
        if !in_date(current) {
            return Err(format!(
                "the certificate of {} was not in date when the document was signed",
                name(current)
            ));
        }
        if trusted.contains(current) {
            return Ok(steps);
        }
        if let Some(root) = trusted
            .iter()
            .find(|root| issued_by(current, root) == Some(true))
        {
            return if in_date(root) {
                steps.push((current, root));
                Ok(steps)
            } else {
                Err(format!(
                    "the trusted certificate of {} was not in date when the document was signed",
                    name(root)
                ))
            };
        }
        let issuer = embedded.iter().find(|candidate| {
            **candidate != current
                && may_issue(candidate)
                && issued_by(current, candidate) == Some(true)
        });
        match issuer {
            Some(issuer) => {
                steps.push((current, *issuer));
                current = issuer;
            }
            None => {
                return Err(format!(
                    "nothing leads from the certificate of {} to a trusted one",
                    name(current)
                ));
            }
        }
    }
    Err("the chain of certificates is too long".to_string())
}

/// Checks one signature against the file. Returns what was established.
fn verify(
    [a, b, c, len]: [usize; 4],
    contents: &[u8],
    file: &[u8],
    trusted: Option<&[Certificate]>,
    revocation: bool,
) -> Value {
    let mut out = json!({
        "trust": "not checked: give the certificates you trust to have the signer's chain checked against them",
    });
    let (Some(first), Some(second)) = (
        file.get(a..a.saturating_add(b)),
        c.checked_add(len).and_then(|end| file.get(c..end)),
    ) else {
        out["problem"] = json!("the byte range lies outside the file");
        out["valid"] = json!(false);
        return out;
    };
    // A signature that stops short of the end leaves room for content added after signing.
    out["covers_whole_document"] = json!(a == 0 && c + len == file.len());

    let parsed = der_length(contents)
        .and_then(|n| contents.get(..n))
        .and_then(|der| ContentInfo::from_der(der).ok())
        .filter(|info| info.content_type == SIGNED_DATA)
        .and_then(|info| info.content.decode_as::<SignedData>().ok());
    let Some(data) = parsed else {
        out["problem"] = json!("the signature is not a readable CMS structure");
        out["valid"] = json!(false);
        return out;
    };
    let Some(info) = data.signer_infos.0.iter().next() else {
        out["problem"] = json!("the signature names no signer");
        out["valid"] = json!(false);
        return out;
    };

    let digest: Vec<u8> = match info.digest_alg.oid {
        oid if oid == SHA256 => Sha256::new()
            .chain_update(first)
            .chain_update(second)
            .finalize()
            .to_vec(),
        oid if oid == SHA384 => Sha384::new()
            .chain_update(first)
            .chain_update(second)
            .finalize()
            .to_vec(),
        oid if oid == SHA512 => Sha512::new()
            .chain_update(first)
            .chain_update(second)
            .finalize()
            .to_vec(),
        oid if oid == SHA1 => {
            out["problem"] = json!("the signature uses SHA-1, which is no longer checked");
            out["valid"] = json!(false);
            return out;
        }
        other => {
            out["problem"] = json!(format!("unsupported digest algorithm {other}"));
            out["valid"] = json!(false);
            return out;
        }
    };
    let attribute = |oid: ObjectIdentifier| {
        info.signed_attrs
            .as_ref()
            .and_then(|attrs| attrs.iter().find(|a| a.oid == oid))
            .and_then(|a| a.values.iter().next())
    };
    let stated = attribute(MESSAGE_DIGEST).map(|v| v.value().to_vec());
    // Unchanged since signing: the bytes still hash to what the signer attested.
    let unchanged = stated.as_deref() == Some(&digest[..]);
    out["document_unchanged"] = json!(unchanged);
    if let Some(time) =
        attribute(SIGNING_TIME).and_then(|v| v.decode_as::<der::asn1::UtcTime>().ok())
    {
        out["signed_at"] = json!(pdf_date(time.to_unix_duration().as_secs()));
    }

    // What a timestamp authority stated, if one was asked: the time, and that its
    // statement is about this very signature.
    let stamp = info
        .unsigned_attrs
        .as_ref()
        .and_then(|attrs| attrs.iter().find(|a| a.oid == TIMESTAMP_TOKEN))
        .and_then(|a| a.values.iter().next())
        .and_then(read_timestamp);
    if let Some(stamp) = &stamp {
        out["timestamp"] = json!({
            "time": pdf_date(stamp.time),
            "about_this_signature": stamp.about == Sha256::digest(info.signature.as_bytes())[..],
            "authority": "not checked: the timestamp's own signature is taken as it is",
        });
    }
    let certificate = signer_certificate(&data, info);
    if let Some((signer, trusted)) = certificate.zip(trusted) {
        let embedded: Vec<&Certificate> = data
            .certificates
            .iter()
            .flat_map(|set| set.0.iter())
            .filter_map(|choice| match choice {
                CertificateChoices::Certificate(cert) => Some(cert),
                _ => None,
            })
            .collect();
        // Judged at the time the signer states; without one, as of now.
        let at = attribute(SIGNING_TIME)
            .and_then(|v| v.decode_as::<der::asn1::UtcTime>().ok())
            .map(|time| time.to_unix_duration().as_secs())
            .or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|now| now.as_secs())
            })
            .unwrap_or(0);
        let chain = chain_of_trust(signer, &embedded, trusted, at);
        out["trusted"] = json!(chain.is_ok());
        out["trust"] = json!(match &chain {
            Ok(_) => "the signer's certificate leads to one you trust".to_string(),
            Err(why) => format!("not trusted: {why}"),
        });
        out["revocation"] = json!(match (&chain, revocation) {
            (Ok(steps), true) => match check_revocation(steps) {
                Ok(()) => "none of the certificates on the way is on its issuer's revocation list"
                    .to_string(),
                Err(why) => {
                    if why.ends_with("was revoked") {
                        out["trusted"] = json!(false);
                    }
                    why
                }
            },
            _ => "not checked".to_string(),
        });
    }
    if let Some(cert) = certificate {
        let tbs = &cert.tbs_certificate;
        out["certificate"] = json!({
            "subject": tbs.subject.to_string(),
            "issuer": tbs.issuer.to_string(),
            "valid_from": pdf_date(tbs.validity.not_before.to_unix_duration().as_secs()),
            "valid_to": pdf_date(tbs.validity.not_after.to_unix_duration().as_secs()),
            "self_signed": tbs.subject == tbs.issuer,
        });
        out["signer"] = json!(common_name(&tbs.subject));
    }
    // The signature itself: made by the certificate's key over the signed attributes.
    let genuine = certificate
        .zip(info.signed_attrs.as_ref())
        .and_then(|(cert, attrs)| {
            let message = attrs.to_der().ok()?;
            let spki = cert.tbs_certificate.subject_public_key_info.to_der().ok()?;
            let signature = info.signature.as_bytes();
            let algorithm = cert.tbs_certificate.subject_public_key_info.algorithm.oid;
            if algorithm == RSA {
                let key = rsa::RsaPublicKey::from_public_key_der(&spki).ok()?;
                let signature = rsa::pkcs1v15::Signature::try_from(signature).ok()?;
                Some(match info.digest_alg.oid {
                    oid if oid == SHA384 => rsa::pkcs1v15::VerifyingKey::<Sha384>::new(key)
                        .verify(&message, &signature)
                        .is_ok(),
                    oid if oid == SHA512 => rsa::pkcs1v15::VerifyingKey::<Sha512>::new(key)
                        .verify(&message, &signature)
                        .is_ok(),
                    _ => rsa::pkcs1v15::VerifyingKey::<Sha256>::new(key)
                        .verify(&message, &signature)
                        .is_ok(),
                })
            } else if algorithm == EC && info.digest_alg.oid == SHA256 {
                let key = p256::ecdsa::VerifyingKey::from_public_key_der(&spki).ok()?;
                let signature = p256::ecdsa::DerSignature::from_bytes(signature).ok()?;
                Some(key.verify(&message, &signature).is_ok())
            } else {
                None
            }
        });
    out["signature_genuine"] = json!(genuine);
    out["valid"] = json!(unchanged && genuine == Some(true));
    if genuine.is_none() {
        out["problem"] =
            json!("the signature algorithm is not one pdfops can check (RSA or ECDSA P-256)");
    }
    out
}

/// The certificate the signer info refers to, among those embedded.
fn signer_certificate<'a>(data: &'a SignedData, info: &SignerInfo) -> Option<&'a Certificate> {
    let certs: Vec<&Certificate> = data
        .certificates
        .as_ref()?
        .0
        .iter()
        .filter_map(|c| match c {
            CertificateChoices::Certificate(cert) => Some(cert),
            _ => None,
        })
        .collect();
    match &info.sid {
        SignerIdentifier::IssuerAndSerialNumber(id) => certs
            .iter()
            .find(|c| {
                c.tbs_certificate.issuer == id.issuer
                    && c.tbs_certificate.serial_number == id.serial_number
            })
            .copied(),
        _ => certs.first().copied(),
    }
}

pub fn signatures(a: SignaturesArgs) -> Result<Value> {
    let file =
        std::fs::read(&a.input).map_err(|e| anyhow!("cannot read {}: {e}", a.input.display()))?;
    // What the signer wrote about the signature is a courtesy; it comes from the parsed
    // document when that still opens.
    let parsed = doc::read(&a.input, a.password.as_deref()).ok();
    let stated: Vec<(String, &Dictionary)> = parsed
        .as_ref()
        .map(|(d, _)| signature_dicts(d))
        .unwrap_or_default();
    let trusted = a
        .trust
        .as_ref()
        .map(|path| {
            let pem =
                std::fs::read(path).map_err(|e| anyhow!("cannot read {}: {e}", path.display()))?;
            Certificate::load_pem_chain(&pem)
                .map_err(|e| anyhow!("{} is not a PEM certificate: {e}", path.display()))
        })
        .transpose()?;
    let found: Vec<Value> = raw_signatures(&file)
        .into_iter()
        .map(|(range, contents)| {
            let mut entry = verify(range, &contents, &file, trusted.as_deref(), a.revocation);
            let own = parsed.as_ref().zip(stated.iter().find(|(_, sig)| {
                sig.get(b"ByteRange")
                    .ok()
                    .and_then(|r| r.as_array().ok())
                    .is_some_and(|r| {
                        r.iter()
                            .filter_map(|o| o.as_i64().ok())
                            .map(|n| n as usize)
                            .eq(range)
                    })
            }));
            if let Some(((d, _), (field, sig))) = own {
                entry["field"] = json!(field);
                for (name, key) in [
                    ("reason", &b"Reason"[..]),
                    ("location", b"Location"),
                    ("sub_filter", b"SubFilter"),
                ] {
                    if let Some(value) = sig
                        .get(key)
                        .ok()
                        .and_then(|o| doc::text(doc::resolve(d, o)))
                    {
                        entry[name] = json!(value);
                    }
                }
                if entry.get("signed_at").is_none() {
                    entry["signed_at"] = json!(
                        sig.get(b"M")
                            .ok()
                            .and_then(|o| doc::text(doc::resolve(d, o)))
                    );
                }
            }
            entry
        })
        .collect();
    let valid = found.iter().filter(|s| s["valid"] == true).count();
    Ok(json!({"file": a.input, "signatures": found.len(), "valid": valid, "details": found}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_convert_from_unix_time() {
        assert_eq!(pdf_date(0), "D:19700101000000Z");
        assert_eq!(pdf_date(951_782_400), "D:20000229000000Z");
        assert_eq!(pdf_date(1_791_203_696), "D:20261005123456Z");
        assert_eq!(civil(-1), (1969, 12, 31));
    }

    #[test]
    fn der_length_reads_short_and_long_forms() {
        assert_eq!(der_length(&[0x30, 0x03, 1, 2, 3, 0, 0]), Some(5));
        assert_eq!(der_length(&[0x30, 0x82, 0x01, 0x00]), Some(260));
        assert_eq!(der_length(&[0x30]), None);
    }
}
