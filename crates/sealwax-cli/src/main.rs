//! `sealwax`: inspect PDF documents from the command line.

mod annots;
mod census;
mod corpus;
mod page;
mod pages;
mod pdfa;
mod save;
mod sign;
mod text;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use sealwax::{Document, OpenOptions, PageGeometry};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(
    name = "sealwax",
    version,
    about = "Inspect PDF documents with sealwax"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the page count and the geometry of every page, as JSON.
    Info {
        /// The PDF to read.
        file: PathBuf,
        /// Password of an encrypted document.
        #[arg(long, default_value = "")]
        password: String,
    },
    /// Survey every PDF under the given files or directories: one JSON line per document.
    Census {
        /// Files or directories (searched recursively for *.pdf).
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// JSON lines output.
        #[arg(long)]
        out: PathBuf,
        /// Documents surveyed at once.
        #[arg(long, default_value_t = 8)]
        jobs: usize,
    },
    /// Page count and geometry of every PDF under the given files or directories, one JSON line
    /// per document.
    Pages {
        /// Files or directories (searched recursively for *.pdf).
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// JSON lines output.
        #[arg(long)]
        out: PathBuf,
        /// Documents read at once.
        #[arg(long, default_value_t = 8)]
        jobs: usize,
    },
    /// Save every PDF under the given files or directories, for the judges of
    /// `benchmark/judges/`: outputs named by the SHA-256 of their input, one JSON line each.
    Save {
        /// Files or directories (searched recursively for *.pdf).
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Where the saved PDFs go.
        #[arg(long)]
        out_dir: PathBuf,
        /// How to save.
        #[arg(long, value_enum, default_value = "stream")]
        mode: save::Mode,
        /// JSON lines output.
        #[arg(long)]
        out: PathBuf,
        /// Documents saved at once.
        #[arg(long, default_value_t = 8)]
        jobs: usize,
    },
    /// Convert every PDF under the given files or directories to PDF/A-3: outputs named by the
    /// SHA-256 of their input, one JSON line each with what was changed and what was left.
    Pdfa {
        /// Files or directories (searched recursively for *.pdf).
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Where the converted PDFs go.
        #[arg(long)]
        out_dir: PathBuf,
        /// The conformance level to declare.
        #[arg(long, value_enum, default_value = "b")]
        level: pdfa::Conformance,
        /// At level a, the language of documents that declare none (`fr-CA`); else it is told
        /// from their words when they leave no doubt.
        #[arg(long)]
        lang: Option<String>,
        /// JSON lines output.
        #[arg(long)]
        out: PathBuf,
        /// Documents converted at once.
        #[arg(long, default_value_t = 8)]
        jobs: usize,
    },
    /// Flatten the annotations of every PDF under the given files or directories: outputs named by
    /// the SHA-256 of their input, one JSON line each.
    Flatten {
        /// Files or directories (searched recursively for *.pdf).
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Where the flattened PDFs go.
        #[arg(long)]
        out_dir: PathBuf,
        /// Then convert to PDF/A-3 at this level.
        #[arg(long, value_enum)]
        pdfa: Option<pdfa::Conformance>,
        /// JSON lines output.
        #[arg(long)]
        out: PathBuf,
        /// Documents processed at once.
        #[arg(long, default_value_t = 8)]
        jobs: usize,
    },
    /// Merge PDFs into one, in the given order, each flattened first.
    Merge {
        /// The PDFs, in order.
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// The merged PDF.
        #[arg(long)]
        out: PathBuf,
    },
    /// Apply to every PDF under the given files or directories the XFDF `<xfdf-dir>/<sha256>.xfdf`
    /// of the same SHA-256, flattening first, as the final PDF task does.
    Xfdf {
        /// Files or directories (searched recursively for *.pdf).
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Where the XFDF are, named by the SHA-256 of their PDF.
        #[arg(long)]
        xfdf_dir: PathBuf,
        /// Where the output PDFs go.
        #[arg(long)]
        out_dir: PathBuf,
        /// Then convert to PDF/A-3 at this level.
        #[arg(long, value_enum)]
        pdfa: Option<pdfa::Conformance>,
        /// JSON lines output.
        #[arg(long)]
        out: PathBuf,
        /// Documents processed at once.
        #[arg(long, default_value_t = 8)]
        jobs: usize,
    },
    /// What the glyphs of every PDF under the given files or directories map to in Unicode (PDF/A
    /// clauses 6.2.11.7.2 and 6.2.11.7.3), one JSON line per document.
    TextCensus {
        /// Files or directories (searched recursively for *.pdf).
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// JSON lines output.
        #[arg(long)]
        out: PathBuf,
        /// Documents read at once.
        #[arg(long, default_value_t = 8)]
        jobs: usize,
    },
    /// Markdown report of a text-census run.
    TextCensusReport {
        /// The JSON lines a text-census run wrote.
        results: PathBuf,
    },
    /// Markdown report of a census run.
    CensusReport {
        /// The JSON lines a census run wrote.
        results: PathBuf,
    },
    /// Append a signature to a document, in an incremental revision: an invisible field on the
    /// first page, a certification (DocMDP P=3) or an approval. Its value is made by the signer
    /// command, as an HSM makes it: the digest to sign in hexadecimal on its standard input, the
    /// signature value in hexadecimal on its standard output. With `--tsa`, the value is
    /// timestamped (RFC 3161): the TSA command reads the request (DER) on its standard input and
    /// writes the response (DER) on its standard output; what became of the timestamp is printed
    /// as JSON.
    Sign(Box<sign::Arguments>),
    /// Add a signature's validation data (PAdES B-LT) in an incremental revision: a proof that
    /// each certificate of the signer's and the timestamp authority's paths was not revoked, an
    /// OCSP response or a CRL, checked, then written into the document security store with the
    /// signature's VRI entry. The fetch command gets `ocsp URL` (the request, DER, on its standard
    /// input) or `crl URL`, and writes the response or the CRL (DER) on its standard output. What
    /// was added is printed as JSON.
    Ltv(Box<sign::LtvArguments>),
    /// Add a document timestamp (PAdES B-LTA) in an incremental revision: an RFC 3161 token over
    /// the whole document, in an invisible field. The TSA command reads the request (DER) on its
    /// standard input and writes the response (DER) on its standard output. Given after the
    /// signatures' validation data, it keeps that data provable once the certificates expire.
    Timestamp(Box<sign::TimestampArguments>),
    /// Check the integrity of the signatures of each PDF: the byte range covers the revision,
    /// the digest matches, the value verifies, and how many revisions followed. Whom the signers
    /// are is not looked at. One JSON line per signature.
    Verify {
        /// The PDFs.
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Write the glyphs every page draws, in the order of its content: one JSON object per glyph
    /// and line (its page from 1, its text, origin, advance, box, size, direction, font, and
    /// whether it is invisible, drawn over itself, replaced by an /ActualText, or an
    /// annotation's).
    Glyphs {
        /// The PDF to read.
        file: PathBuf,
        /// Also the glyphs the annotations' appearances draw.
        #[arg(long)]
        annotations: bool,
        /// Where to write them (standard output without it).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Password of an encrypted document.
        #[arg(long, default_value = "")]
        password: String,
    },
    /// Render one page to a PNG image (8-bit RGB): its crop box as shown, annotations included.
    Render {
        /// The PDF to read.
        file: PathBuf,
        /// The page, from 1.
        #[arg(long, default_value_t = 1)]
        page: usize,
        /// Pixels per point (150 / 72 = 2.0833 for 150 dpi).
        #[arg(long, default_value_t = 1.0)]
        scale: f64,
        /// The PNG to write.
        #[arg(long)]
        out: PathBuf,
        /// Password of an encrypted document.
        #[arg(long, default_value = "")]
        password: String,
    },
}

#[derive(Debug, Serialize)]
struct Info {
    page_count: usize,
    pages: Vec<PageGeometry>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Sign(arguments) => match sign::run(&arguments) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("sealwax: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Ltv(arguments) => match sign::run_ltv(&arguments) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("sealwax: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Timestamp(arguments) => match sign::run_timestamp(&arguments) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("sealwax: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Glyphs {
            file,
            annotations,
            out,
            password,
        } => match page::glyphs(&file, &password, annotations, out.as_deref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("sealwax: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Render {
            file,
            page,
            scale,
            out,
            password,
        } => match page::render(&file, &password, page, scale, &out) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("sealwax: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Verify { files } => match sign::run_verify(&files) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::from(2),
            Err(error) => {
                eprintln!("sealwax: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Census { paths, out, jobs } => match census::run(&paths, &out, jobs) {
            Ok(n) => {
                eprintln!("sealwax: {n} documents surveyed into {}", out.display());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("sealwax: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Pages { paths, out, jobs } => match pages::run(&paths, &out, jobs) {
            Ok(n) => {
                eprintln!("sealwax: {n} documents measured into {}", out.display());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("sealwax: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Save {
            paths,
            out_dir,
            mode,
            out,
            jobs,
        } => match save::run(&paths, &out_dir, mode, &out, jobs) {
            Ok(n) => {
                eprintln!("sealwax: {n} documents saved into {}", out_dir.display());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("sealwax: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Pdfa {
            paths,
            out_dir,
            level,
            lang,
            out,
            jobs,
        } => match pdfa::run(&paths, &out_dir, level, lang.as_deref(), &out, jobs) {
            Ok(n) => {
                eprintln!(
                    "sealwax: {n} documents converted into {}",
                    out_dir.display()
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("sealwax: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Flatten {
            paths,
            out_dir,
            pdfa,
            out,
            jobs,
        } => match annots::run_flatten(&paths, &out_dir, pdfa, &out, jobs) {
            Ok(n) => {
                eprintln!(
                    "sealwax: {n} documents flattened into {}",
                    out_dir.display()
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("sealwax: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Merge { inputs, out } => match annots::merge(&inputs, &out) {
            Ok(pages) => {
                eprintln!("sealwax: {pages} pages merged into {}", out.display());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("sealwax: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Xfdf {
            paths,
            xfdf_dir,
            out_dir,
            pdfa,
            out,
            jobs,
        } => match annots::run_xfdf(&paths, &xfdf_dir, &out_dir, pdfa, &out, jobs) {
            Ok(n) => {
                eprintln!(
                    "sealwax: XFDF applied to {n} documents into {}",
                    out_dir.display()
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("sealwax: {error}");
                ExitCode::FAILURE
            }
        },
        Command::TextCensus { paths, out, jobs } => match text::run(&paths, &out, jobs) {
            Ok(n) => {
                eprintln!("sealwax: {n} documents read into {}", out.display());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("sealwax: {error}");
                ExitCode::FAILURE
            }
        },
        Command::TextCensusReport { results } => match text::report(&results) {
            Ok(report) => {
                print!("{report}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("sealwax: {error}");
                ExitCode::FAILURE
            }
        },
        Command::CensusReport { results } => match census::report(&results) {
            Ok(report) => {
                print!("{report}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("sealwax: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Info { file, password } => {
            match Document::open_path(&file, &OpenOptions::default().password(password)) {
                Ok(doc) => {
                    let info = Info {
                        page_count: doc.page_count(),
                        pages: doc.geometry(),
                    };
                    match serde_json::to_string_pretty(&info) {
                        Ok(json) => {
                            println!("{json}");
                            ExitCode::SUCCESS
                        }
                        Err(error) => {
                            eprintln!("sealwax: {error}");
                            ExitCode::FAILURE
                        }
                    }
                }
                Err(error) => {
                    eprintln!("sealwax: {}: {error}", file.display());
                    ExitCode::FAILURE
                }
            }
        }
    }
}
