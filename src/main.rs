//! pdfskim — compress PDFs and strip digital signatures, built on PdfCraft's PDF libraries.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use pdfcraft_optimize::{Compression, MonoCompression, Preset};

use pdfskim::compress::OptimizeOptions;
use pdfskim::run::{self, InfoOptions, JobOptions, Mode};

const EXAMPLES: &str = "\
Examples:
  pdfskim compress report.pdf
  pdfskim compress scans/ --recursive --out-dir compressed
  pdfskim compress a.pdf b.pdf c.pdf --out-dir out --quality 45 --ppi 120
  pdfskim strip signed.pdf
  pdfskim clean signed.pdf -o clean.pdf
  pdfskim info signed.pdf --json
";

#[derive(Parser)]
#[command(
    name = "pdfskim",
    version,
    about = "Compress PDFs and remove digital signatures, using PdfCraft's optimizer",
    after_help = EXAMPLES,
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compress PDFs (Acrobat's Reduce File Size / PDF Optimizer, as implemented by PdfCraft)
    Compress(CompressArgs),
    /// Remove digital signatures and certification, then rewrite the file
    Strip(CommonArgs),
    /// Remove digital signatures first, then compress
    Clean(CompressArgs),
    /// Show size, encryption and signature information
    Info(InfoArgs),
}

#[derive(Args)]
struct CommonArgs {
    /// PDF files, folders or wildcard patterns such as *.pdf to process
    #[arg(value_name = "INPUT", required = true, num_args = 1..)]
    inputs: Vec<PathBuf>,

    /// Write a single input to this file (one input only)
    #[arg(short = 'o', long, value_name = "FILE")]
    out: Option<PathBuf>,

    /// Write outputs under this folder; folders are searched inside it
    #[arg(long, value_name = "DIR")]
    out_dir: Option<PathBuf>,

    /// Search folders recursively (otherwise only their direct children)
    #[arg(short = 'r', long)]
    recursive: bool,

    /// Replace existing output files
    #[arg(long)]
    overwrite: bool,

    /// Password for encrypted PDFs (or set PDFSKIM_PASSWORD)
    #[arg(long, value_name = "PASSWORD")]
    password: Option<String>,

    /// Largest input accepted, in MiB (PdfCraft's batch limit is 512)
    #[arg(long, value_name = "MIB", default_value_t = 512, value_parser = clap::value_parser!(u64).range(1..=4096))]
    max_input_mib: u64,

    /// Print one JSON report to stdout
    #[arg(long)]
    json: bool,

    /// Do not print per-file progress
    #[arg(short = 'q', long)]
    quiet: bool,

    /// Run everything but write no output
    #[arg(long)]
    dry_run: bool,

    /// Stop the batch after the first failed file
    #[arg(long)]
    stop_on_error: bool,
}

#[derive(Args)]
struct CompressArgs {
    #[command(flatten)]
    common: CommonArgs,
    #[command(flatten)]
    optimize: OptimizeArgs,
}

#[derive(Args)]
struct OptimizeArgs {
    /// Optimizer preset
    #[arg(long, value_enum, default_value_t = PresetArg::Standard)]
    preset: PresetArg,

    /// Target PDF version: retain, 1.4, 1.5, 1.6, 1.7 or 2.0
    #[arg(long, value_name = "VERSION")]
    compatibility: Option<String>,

    /// JPEG quality for colour and grey images (1-100; preset default 60/80)
    #[arg(long, value_name = "1-100", value_parser = parse_quality)]
    quality: Option<u8>,

    /// Target resolution for colour and grey images, in ppi (9-2400)
    #[arg(long, value_name = "PPI", value_parser = parse_ppi)]
    ppi: Option<f64>,

    /// Downsample colour and grey images above this resolution (default 1.5 x --ppi)
    #[arg(long, value_name = "PPI", value_parser = parse_ppi)]
    above_ppi: Option<f64>,

    /// Target resolution for black-and-white images (default max(--ppi, 300))
    #[arg(long, value_name = "PPI", value_parser = parse_ppi)]
    mono_ppi: Option<f64>,

    /// Downsample black-and-white images above this resolution (default 1.5 x --mono-ppi)
    #[arg(long, value_name = "PPI", value_parser = parse_ppi)]
    mono_above_ppi: Option<f64>,

    /// Colour image compression
    #[arg(long, value_enum)]
    color_compression: Option<ImageCompressionArg>,

    /// Greyscale image compression
    #[arg(long, value_enum)]
    gray_compression: Option<ImageCompressionArg>,

    /// Black-and-white image compression
    #[arg(long, value_enum)]
    mono_compression: Option<MonoCompressionArg>,

    /// Keep every image at its resolution (same as the three --no-*-downsample flags)
    #[arg(long)]
    no_downsample: bool,

    /// Keep colour images at their resolution
    #[arg(long)]
    no_color_downsample: bool,

    /// Keep greyscale images at their resolution
    #[arg(long)]
    no_gray_downsample: bool,

    /// Keep black-and-white images at their resolution
    #[arg(long)]
    no_mono_downsample: bool,

    /// Do not merge page pictures drawn as stacked one-pixel strips
    #[arg(long)]
    no_merge_fragments: bool,

    /// Keep page thumbnails
    #[arg(long)]
    keep_thumbnails: bool,

    /// Keep alternate images
    #[arg(long)]
    keep_alternate_images: bool,

    /// Discard the document's tags (structure tree)
    #[arg(long)]
    discard_tags: bool,

    /// Discard print settings (scaling, duplex, page range, copies)
    #[arg(long)]
    discard_print_settings: bool,

    /// Do not Flate-compress streams that have no filter
    #[arg(long)]
    no_flate: bool,

    /// Leave LZW streams as LZW instead of re-encoding them as Flate
    #[arg(long)]
    no_lzw_to_flate: bool,

    /// Write a classic cross-reference table instead of object streams (PDF 1.5+)
    #[arg(long)]
    no_object_streams: bool,

    /// Keep links and bookmark destinations that point nowhere
    #[arg(long)]
    keep_invalid_links: bool,

    /// Keep named destinations nothing refers to
    #[arg(long)]
    keep_unreferenced_dests: bool,

    /// Remove digital signatures first, then compress (same as `pdfskim clean`)
    #[arg(long)]
    strip_signatures: bool,
}

#[derive(Args)]
struct InfoArgs {
    /// PDF files to describe
    #[arg(value_name = "INPUT", required = true, num_args = 1..)]
    inputs: Vec<PathBuf>,

    /// Password for encrypted PDFs (or set PDFSKIM_PASSWORD)
    #[arg(long, value_name = "PASSWORD")]
    password: Option<String>,

    /// Print JSON instead of text
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum PresetArg {
    Standard,
    Mobile,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ImageCompressionArg {
    Jpeg,
    Zip,
    Retain,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum MonoCompressionArg {
    /// CCITT Group 4 (lossless, best for scans)
    CcittG4,
    /// JBIG2 (lossless, best for repeated marks and text)
    Jbig2,
    Retain,
}

fn parse_quality(text: &str) -> Result<u8, String> {
    let value: u8 = text
        .parse()
        .map_err(|_| format!("{text:?} is not a whole number"))?;
    if (1..=100).contains(&value) {
        Ok(value)
    } else {
        Err(format!("{value} is outside 1-100"))
    }
}

fn parse_ppi(text: &str) -> Result<f64, String> {
    let value: f64 = text
        .parse()
        .map_err(|_| format!("{text:?} is not a number"))?;
    if value.is_finite() && (9.0..=2400.0).contains(&value) {
        Ok(value)
    } else {
        Err(format!("{value} is outside 9-2400"))
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match execute(cli) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("pdfskim: {}", pdfskim::util::describe_error(&error));
            ExitCode::from(1)
        }
    }
}

fn execute(cli: Cli) -> anyhow::Result<u8> {
    match cli.command {
        Command::Compress(args) => {
            let mode = if args.optimize.strip_signatures {
                Mode::Clean
            } else {
                Mode::Compress
            };
            run::execute(job(args.common, mode, args.optimize))
        }
        Command::Strip(common) => run::execute(job(common, Mode::Strip, OptimizeArgs::default())),
        Command::Clean(args) => run::execute(job(args.common, Mode::Clean, args.optimize)),
        Command::Info(args) => run::info(InfoOptions {
            inputs: args.inputs,
            password: password(args.password),
            json: args.json,
        }),
    }
}

fn job(common: CommonArgs, mode: Mode, optimize: OptimizeArgs) -> JobOptions {
    JobOptions {
        mode,
        inputs: common.inputs,
        out: common.out,
        out_dir: common.out_dir,
        recursive: common.recursive,
        overwrite: common.overwrite,
        password: password(common.password),
        max_input_mib: common.max_input_mib,
        json: common.json,
        quiet: common.quiet,
        dry_run: common.dry_run,
        stop_on_error: common.stop_on_error,
        optimize: optimize.into(),
    }
}

fn password(given: Option<String>) -> Option<String> {
    given
        .or_else(|| std::env::var("PDFSKIM_PASSWORD").ok())
        .filter(|value| !value.is_empty())
}

impl Default for OptimizeArgs {
    fn default() -> Self {
        Self {
            preset: PresetArg::Standard,
            compatibility: None,
            quality: None,
            ppi: None,
            above_ppi: None,
            mono_ppi: None,
            mono_above_ppi: None,
            color_compression: None,
            gray_compression: None,
            mono_compression: None,
            no_downsample: false,
            no_color_downsample: false,
            no_gray_downsample: false,
            no_mono_downsample: false,
            no_merge_fragments: false,
            keep_thumbnails: false,
            keep_alternate_images: false,
            discard_tags: false,
            discard_print_settings: false,
            no_flate: false,
            no_lzw_to_flate: false,
            no_object_streams: false,
            keep_invalid_links: false,
            keep_unreferenced_dests: false,
            strip_signatures: false,
        }
    }
}

impl From<OptimizeArgs> for OptimizeOptions {
    fn from(args: OptimizeArgs) -> Self {
        Self {
            preset: Some(match args.preset {
                PresetArg::Standard => Preset::Standard,
                PresetArg::Mobile => Preset::Mobile,
            }),
            compatibility: args.compatibility,
            quality: args.quality,
            ppi: args.ppi,
            above_ppi: args.above_ppi,
            mono_ppi: args.mono_ppi,
            mono_above_ppi: args.mono_above_ppi,
            color_compression: args.color_compression.map(image_compression),
            gray_compression: args.gray_compression.map(image_compression),
            mono_compression: args.mono_compression.map(mono_compression),
            no_downsample: args.no_downsample,
            no_color_downsample: args.no_color_downsample,
            no_gray_downsample: args.no_gray_downsample,
            no_mono_downsample: args.no_mono_downsample,
            no_merge_fragments: args.no_merge_fragments,
            keep_thumbnails: args.keep_thumbnails,
            keep_alternate_images: args.keep_alternate_images,
            discard_tags: args.discard_tags,
            discard_print_settings: args.discard_print_settings,
            no_flate: args.no_flate,
            no_lzw_to_flate: args.no_lzw_to_flate,
            no_object_streams: args.no_object_streams,
            keep_invalid_links: args.keep_invalid_links,
            keep_unreferenced_dests: args.keep_unreferenced_dests,
        }
    }
}

fn image_compression(value: ImageCompressionArg) -> Compression {
    match value {
        ImageCompressionArg::Jpeg => Compression::Jpeg(60),
        ImageCompressionArg::Zip => Compression::Flate,
        ImageCompressionArg::Retain => Compression::Retain,
    }
}

fn mono_compression(value: MonoCompressionArg) -> MonoCompression {
    match value {
        MonoCompressionArg::CcittG4 => MonoCompression::CcittG4,
        MonoCompressionArg::Jbig2 => MonoCompression::Jbig2,
        MonoCompressionArg::Retain => MonoCompression::Retain,
    }
}
