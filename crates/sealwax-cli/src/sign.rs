//! `sealwax sign`: one signature appended to a document, its value made by an outside command,
//! as an HSM makes it: the command reads the digest to sign in hexadecimal on its standard input
//! and writes the signature value in hexadecimal on its standard output. A timestamp, if asked
//! for, comes from another command: the request (DER) on its standard input, the response (DER)
//! on its standard output.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use clap::ValueEnum;
use sealwax_sign::{
    Algorithm, Integrity, Kind, Options, Policy, RevocationFetcher, Signer, TimestampClient, Trust,
    Validation,
};

/// How the signer signs, by its JOSE name (RFC 7518), as cloud key vaults name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum KeyAlgorithm {
    /// RSA, PKCS #1 v1.5 with SHA-256.
    #[value(alias = "rsa")]
    Rs256,
    /// RSA, PKCS #1 v1.5 with SHA-384.
    Rs384,
    /// RSA, PKCS #1 v1.5 with SHA-512.
    Rs512,
    /// RSA-PSS with SHA-256.
    Ps256,
    /// RSA-PSS with SHA-384.
    Ps384,
    /// RSA-PSS with SHA-512.
    Ps512,
    /// ECDSA with SHA-256.
    #[value(alias = "ecdsa")]
    Es256,
    /// ECDSA with SHA-384.
    Es384,
    /// ECDSA with SHA-512.
    Es512,
}

impl KeyAlgorithm {
    fn algorithm(self) -> Algorithm {
        match self {
            Self::Rs256 => Algorithm::RsaPkcs1Sha256,
            Self::Rs384 => Algorithm::RsaPkcs1Sha384,
            Self::Rs512 => Algorithm::RsaPkcs1Sha512,
            Self::Ps256 => Algorithm::RsaPssSha256,
            Self::Ps384 => Algorithm::RsaPssSha384,
            Self::Ps512 => Algorithm::RsaPssSha512,
            Self::Es256 => Algorithm::EcdsaSha256,
            Self::Es384 => Algorithm::EcdsaSha384,
            Self::Es512 => Algorithm::EcdsaSha512,
        }
    }
}

/// What to sign and how.
#[derive(Debug, clap::Args)]
pub(crate) struct Arguments {
    /// The PDF to sign.
    pub(crate) input: PathBuf,
    /// The signed PDF.
    #[arg(long)]
    pub(crate) out: PathBuf,
    /// The signer's certificate, PEM or DER.
    #[arg(long)]
    pub(crate) cert: PathBuf,
    /// The rest of its chain, PEM or DER: intermediates, the root if wanted.
    #[arg(long)]
    pub(crate) chain: Vec<PathBuf>,
    /// How the signer signs; the digest it gets is that of the algorithm (32, 48 or 64 bytes).
    #[arg(long, value_enum, default_value = "rs256")]
    pub(crate) algorithm: KeyAlgorithm,
    /// The name of the new signature field.
    #[arg(long)]
    pub(crate) field: String,
    /// Certify the document (the first signature) rather than approve it.
    #[arg(long)]
    pub(crate) certify: bool,
    #[arg(long)]
    pub(crate) reason: Option<String>,
    #[arg(long)]
    pub(crate) location: Option<String>,
    #[arg(long)]
    pub(crate) contact: Option<String>,
    /// The signing time `/M` declares, in seconds since 1970 (UTC); now by default.
    #[arg(long)]
    pub(crate) time: Option<i64>,
    /// The timestamp authority's command.
    #[arg(long)]
    pub(crate) tsa: Option<String>,
    /// An argument of the TSA command (repeated).
    #[arg(long = "tsa-arg", requires = "tsa", allow_hyphen_values = true)]
    pub(crate) tsa_args: Vec<String>,
    /// A root the authority must chain to, PEM or DER (repeated).
    #[arg(long = "tsa-root", requires = "tsa")]
    pub(crate) tsa_roots: Vec<PathBuf>,
    /// An intermediate of the authority its tokens may leave out, PEM or DER (repeated).
    #[arg(long = "tsa-chain", requires = "tsa")]
    pub(crate) tsa_chain: Vec<PathBuf>,
    /// Sign without the timestamp when it fails, rather than not sign.
    #[arg(long = "tsa-best-effort", requires = "tsa")]
    pub(crate) tsa_best_effort: bool,
    /// Sign even when a signature already in the document is not intact.
    #[arg(long)]
    pub(crate) allow_broken_signatures: bool,
    /// The signer command and its arguments.
    #[arg(last = true, required = true)]
    pub(crate) signer: Vec<String>,
}

/// Where to add a document timestamp, and whom to trust.
#[derive(Debug, clap::Args)]
pub(crate) struct TimestampArguments {
    /// The PDF to timestamp.
    pub(crate) input: PathBuf,
    /// The timestamped PDF.
    #[arg(long)]
    pub(crate) out: PathBuf,
    /// The name of the new field.
    #[arg(long, default_value = "DocTimeStamp1")]
    pub(crate) field: String,
    /// The timestamp authority's command.
    #[arg(long)]
    pub(crate) tsa: String,
    /// An argument of the TSA command (repeated).
    #[arg(long = "tsa-arg", allow_hyphen_values = true)]
    pub(crate) tsa_args: Vec<String>,
    /// A root the authority must chain to, PEM or DER (repeated).
    #[arg(long = "tsa-root", required = true)]
    pub(crate) tsa_roots: Vec<PathBuf>,
    /// An intermediate of the authority its tokens may leave out, PEM or DER (repeated).
    #[arg(long = "tsa-chain")]
    pub(crate) tsa_chain: Vec<PathBuf>,
}

/// Which signature to add validation data to, and whom to trust.
#[derive(Debug, clap::Args)]
pub(crate) struct LtvArguments {
    /// The signed PDF.
    pub(crate) input: PathBuf,
    /// The PDF with the validation data.
    #[arg(long)]
    pub(crate) out: PathBuf,
    /// The signature's field.
    #[arg(long)]
    pub(crate) field: String,
    /// An anchor, the signer's or its timestamp authority's, PEM or DER (repeated).
    #[arg(long = "root", required = true)]
    pub(crate) roots: Vec<PathBuf>,
    /// An intermediate the signatures may leave out, PEM or DER (repeated).
    #[arg(long)]
    pub(crate) chain: Vec<PathBuf>,
    /// The fetch command.
    #[arg(long)]
    pub(crate) fetch: String,
    /// An argument of the fetch command (repeated), before `ocsp URL` or `crl URL`.
    #[arg(long = "fetch-arg", allow_hyphen_values = true)]
    pub(crate) fetch_args: Vec<String>,
    /// The time the data must be current at, in seconds since 1970 (UTC); now by default.
    #[arg(long)]
    pub(crate) time: Option<i64>,
}

/// A certificate file, PEM or DER, as DER.
fn certificate(path: &Path) -> Result<Vec<u8>, String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !data.starts_with(b"-----BEGIN") {
        return Ok(data);
    }
    let text = String::from_utf8_lossy(&data);
    let body: String = text
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect::<String>()
        .split_whitespace()
        .collect();
    decode_base64(&body).ok_or_else(|| format!("{}: malformed PEM", path.display()))
}

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in text.bytes() {
        if byte == b'=' {
            break;
        }
        let value = ALPHABET.iter().position(|c| *c == byte)?;
        buffer = (buffer << 6) | u32::try_from(value).ok()?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buffer >> bits) & 0xff).ok()?);
        }
    }
    Some(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// `input` written to the standard input of `command`, its standard output read back.
fn run_command(command: &[String], what: &str, input: &[u8]) -> Result<Vec<u8>, String> {
    let (program, arguments) = command
        .split_first()
        .ok_or_else(|| format!("no {what} command"))?;
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{what} command: {e}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| format!("{what} command: no input"))?
        .write_all(input)
        .map_err(|e| format!("{what} command: {e}"))?;
    let output = child
        .wait_with_output()
        .map_err(|e| format!("{what} command: {e}"))?;
    if !output.status.success() {
        return Err(format!("{what} command failed: {}", output.status));
    }
    Ok(output.stdout)
}

/// A timestamp authority reached through a command.
struct ExternalAuthority<'a> {
    command: &'a [String],
}

impl TimestampClient for ExternalAuthority<'_> {
    fn timestamp(&self, request: &[u8]) -> Result<Vec<u8>, String> {
        run_command(self.command, "TSA", request)
    }
}

/// Revocation information fetched through a command.
struct ExternalFetcher<'a> {
    command: &'a [String],
}

impl ExternalFetcher<'_> {
    fn fetch(&self, kind: &str, url: &str, input: &[u8]) -> Result<Vec<u8>, String> {
        let command: Vec<String> = self
            .command
            .iter()
            .cloned()
            .chain([kind.to_owned(), url.to_owned()])
            .collect();
        run_command(&command, "fetch", input)
    }
}

impl RevocationFetcher for ExternalFetcher<'_> {
    fn ocsp(&self, url: &str, request: &[u8]) -> Result<Vec<u8>, String> {
        self.fetch("ocsp", url, request)
    }

    fn crl(&self, url: &str) -> Result<Vec<u8>, String> {
        self.fetch("crl", url, &[])
    }
}

/// A signer that runs a command for each digest.
struct External<'a> {
    certificate: Vec<u8>,
    chain: Vec<Vec<u8>>,
    algorithm: Algorithm,
    command: &'a [String],
}

impl Signer for External<'_> {
    fn certificate(&self) -> &[u8] {
        &self.certificate
    }

    fn chain(&self) -> &[Vec<u8>] {
        &self.chain
    }

    fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    fn sign(&self, _message: &[u8], digest: &[u8]) -> Result<Vec<u8>, String> {
        let output = run_command(self.command, "signer", hex(digest).as_bytes())?;
        unhex(&String::from_utf8_lossy(&output))
            .ok_or_else(|| "signer command wrote no hexadecimal signature".to_owned())
    }
}

pub(crate) fn run(arguments: &Arguments) -> Result<(), String> {
    let pdf = std::fs::read(&arguments.input)
        .map_err(|e| format!("{}: {e}", arguments.input.display()))?;
    let read_all = |paths: &[PathBuf]| -> Result<Vec<Vec<u8>>, String> {
        paths.iter().map(|path| certificate(path)).collect()
    };
    let signer = External {
        certificate: certificate(&arguments.cert)?,
        chain: read_all(&arguments.chain)?,
        algorithm: arguments.algorithm.algorithm(),
        command: &arguments.signer,
    };
    let mut options = Options::new(
        arguments.field.as_str(),
        if arguments.certify {
            Kind::certification()
        } else {
            Kind::Approval
        },
    );
    options.reason.clone_from(&arguments.reason);
    options.location.clone_from(&arguments.location);
    options.contact_info.clone_from(&arguments.contact);
    options.signing_time = arguments.time;
    options.allow_broken_signatures = arguments.allow_broken_signatures;
    let write = |pdf: &[u8]| {
        std::fs::write(&arguments.out, pdf).map_err(|e| format!("{}: {e}", arguments.out.display()))
    };
    let Some(program) = &arguments.tsa else {
        let signed =
            sealwax_sign::sign(&pdf, &signer, &options, None).map_err(|e| e.to_string())?;
        return write(&signed.pdf);
    };
    let trust = Trust::new(
        &read_all(&arguments.tsa_roots)?,
        &read_all(&arguments.tsa_chain)?,
    )
    .map_err(|e| e.to_string())?;
    let command: Vec<String> = std::iter::once(program.clone())
        .chain(arguments.tsa_args.iter().cloned())
        .collect();
    let authority = ExternalAuthority { command: &command };
    let timestamping = sealwax_sign::Timestamping {
        client: &authority,
        trust: &trust,
        policy: if arguments.tsa_best_effort {
            Policy::BestEffort
        } else {
            Policy::Required
        },
    };
    let signed = sealwax_sign::sign(&pdf, &signer, &options, Some(&timestamping))
        .map_err(|e| e.to_string())?;
    write(&signed.pdf)?;
    let report = match (&signed.timestamp, &signed.timestamp_error) {
        (Some(token), _) => serde_json::json!({
            "timestamp": {"time": token.time, "authority": token.authority},
        }),
        (None, Some(error)) => serde_json::json!({"timestamp_error": error.to_string()}),
        (None, None) => serde_json::json!({}),
    };
    println!("{report}");
    Ok(())
}

pub(crate) fn run_ltv(arguments: &LtvArguments) -> Result<(), String> {
    let pdf = std::fs::read(&arguments.input)
        .map_err(|e| format!("{}: {e}", arguments.input.display()))?;
    let read_all = |paths: &[PathBuf]| -> Result<Vec<Vec<u8>>, String> {
        paths.iter().map(|path| certificate(path)).collect()
    };
    let trust = Trust::new(&read_all(&arguments.roots)?, &read_all(&arguments.chain)?)
        .map_err(|e| e.to_string())?;
    let command: Vec<String> = std::iter::once(arguments.fetch.clone())
        .chain(arguments.fetch_args.iter().cloned())
        .collect();
    let fetcher = ExternalFetcher { command: &command };
    let validation = Validation {
        fetcher: &fetcher,
        trust: &trust,
        time: arguments.time,
    };
    let (data, coverage) = sealwax_sign::add_validation_data(&pdf, &arguments.field, &validation)
        .map_err(|e| e.to_string())?;
    std::fs::write(&arguments.out, data)
        .map_err(|e| format!("{}: {e}", arguments.out.display()))?;
    println!(
        "{}",
        serde_json::json!({
            "certificates": coverage.certificates,
            "ocsp_responses": coverage.ocsp_responses,
            "crls": coverage.crls,
        })
    );
    Ok(())
}

pub(crate) fn run_timestamp(arguments: &TimestampArguments) -> Result<(), String> {
    let pdf = std::fs::read(&arguments.input)
        .map_err(|e| format!("{}: {e}", arguments.input.display()))?;
    let read_all = |paths: &[PathBuf]| -> Result<Vec<Vec<u8>>, String> {
        paths.iter().map(|path| certificate(path)).collect()
    };
    let trust = Trust::new(
        &read_all(&arguments.tsa_roots)?,
        &read_all(&arguments.tsa_chain)?,
    )
    .map_err(|e| e.to_string())?;
    let command: Vec<String> = std::iter::once(arguments.tsa.clone())
        .chain(arguments.tsa_args.iter().cloned())
        .collect();
    let authority = ExternalAuthority { command: &command };
    let (data, timestamp) =
        sealwax_sign::add_document_timestamp(&pdf, &arguments.field, &authority, &trust)
            .map_err(|e| e.to_string())?;
    std::fs::write(&arguments.out, data)
        .map_err(|e| format!("{}: {e}", arguments.out.display()))?;
    println!(
        "{}",
        serde_json::json!({"time": timestamp.time, "authority": timestamp.authority})
    );
    Ok(())
}

/// Print the check of each signature of `files`, one JSON line each; whether all are intact.
pub(crate) fn run_verify(files: &[PathBuf]) -> Result<bool, String> {
    let mut all_intact = true;
    for file in files {
        // A file that cannot be read or opened is reported, and the others checked.
        let read = std::fs::read(file)
            .map_err(|e| e.to_string())
            .and_then(|pdf| sealwax_sign::verify_signatures(&pdf).map_err(|e| e.to_string()));
        let checks = match read {
            Ok(checks) => checks,
            Err(error) => {
                all_intact = false;
                println!(
                    "{}",
                    serde_json::json!({"file": file.display().to_string(), "error": error})
                );
                continue;
            }
        };
        for check in checks {
            let (integrity, detail) = match &check.integrity {
                Integrity::Intact => ("intact", None),
                Integrity::Broken(problem) => ("broken", Some(problem.to_string())),
                Integrity::Unchecked(why) => ("unchecked", Some((*why).to_owned())),
                _ => ("unknown", None),
            };
            all_intact &= check.is_intact();
            println!(
                "{}",
                serde_json::json!({
                    "file": file.display().to_string(),
                    "field": check.field,
                    "kind": format!("{:?}", check.kind),
                    "sub_filter": check.sub_filter,
                    "signer": check.signer,
                    "timestamp_time": check.timestamp_time,
                    "later_revisions": check.later_revisions,
                    "integrity": integrity,
                    "detail": detail,
                })
            );
        }
    }
    Ok(all_intact)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{decode_base64, unhex};

    #[test]
    fn base64_and_hexadecimal() {
        assert_eq!(decode_base64("TWFu").unwrap(), b"Man");
        assert_eq!(decode_base64("TWE=").unwrap(), b"Ma");
        assert_eq!(unhex("0aFF\n").unwrap(), [10, 255]);
        assert_eq!(unhex("abc"), None);
    }
}
