//! Compression through PdfCraft's optimizer (`pdfcraft-optimize`, the same engine behind
//! Acrobat-style Reduce File Size / PDF Optimizer in PdfCraft).

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use pdfcraft_cos::{CosError, Document, ObjRef, SaveOptions, pdf_date, write_full};
use pdfcraft_optimize::{
    Compatibility, Compression, ImageSettings, MonoCompression, Preset, Report, Settings, optimize,
};
use pdfcraft_organize::dedupe_resources;

use crate::signatures::{self, StripOutcome};

/// Everything the optimizer accepts, as the CLI exposes it.
#[derive(Clone, Debug, Default)]
pub struct OptimizeOptions {
    pub preset: Option<Preset>,
    pub compatibility: Option<String>,
    pub quality: Option<u8>,
    pub ppi: Option<f64>,
    pub above_ppi: Option<f64>,
    pub mono_ppi: Option<f64>,
    pub mono_above_ppi: Option<f64>,
    pub color_compression: Option<Compression>,
    pub gray_compression: Option<Compression>,
    pub mono_compression: Option<MonoCompression>,
    pub no_downsample: bool,
    pub no_color_downsample: bool,
    pub no_gray_downsample: bool,
    pub no_mono_downsample: bool,
    pub no_merge_fragments: bool,
    pub keep_thumbnails: bool,
    pub keep_alternate_images: bool,
    pub discard_tags: bool,
    pub discard_print_settings: bool,
    pub no_flate: bool,
    pub no_lzw_to_flate: bool,
    pub no_object_streams: bool,
    pub keep_invalid_links: bool,
    pub keep_unreferenced_dests: bool,
}

impl OptimizeOptions {
    pub fn settings(&self) -> Result<Settings> {
        let mut settings = self.preset.unwrap_or(Preset::Standard).settings();
        if let Some(name) = &self.compatibility {
            settings.compatibility = Compatibility::from_name(name).ok_or_else(|| {
                anyhow::anyhow!("unknown compatibility {name:?} (retain, 1.4, 1.5, 1.6, 1.7, 2.0)")
            })?;
        }

        let default_quality = match self.preset.unwrap_or(Preset::Standard) {
            Preset::Standard => 60,
            Preset::Mobile => 80,
        };
        settings.color = apply_images(
            settings.color,
            self.color_compression,
            self.quality,
            default_quality,
        );
        settings.gray = apply_images(
            settings.gray,
            self.gray_compression,
            self.quality,
            default_quality,
        );

        if let Some(ppi) = self.ppi {
            settings.color.target_ppi = clamp_ppi(ppi);
            settings.gray.target_ppi = clamp_ppi(ppi);
        }
        if let Some(ppi) = self.mono_ppi {
            settings.mono.target_ppi = clamp_ppi(ppi);
        } else if let Some(ppi) = self.ppi {
            // PdfCraft's batch command keeps scanned text legible: black-and-white images go to
            // the larger of the requested resolution and 300 ppi.
            settings.mono.target_ppi = clamp_ppi(ppi.max(300.0));
        }

        if let Some(above) = self.above_ppi {
            settings.color.above_ppi = clamp_ppi(above);
            settings.gray.above_ppi = clamp_ppi(above);
        } else if self.ppi.is_some() {
            settings.color.above_ppi = settings.color.target_ppi * 1.5;
            settings.gray.above_ppi = settings.gray.target_ppi * 1.5;
        }
        if let Some(above) = self.mono_above_ppi {
            settings.mono.above_ppi = clamp_ppi(above);
        } else if self.ppi.is_some() || self.mono_ppi.is_some() {
            settings.mono.above_ppi = settings.mono.target_ppi * 1.5;
        }
        settings.color.above_ppi = settings.color.above_ppi.max(settings.color.target_ppi);
        settings.gray.above_ppi = settings.gray.above_ppi.max(settings.gray.target_ppi);
        settings.mono.above_ppi = settings.mono.above_ppi.max(settings.mono.target_ppi);

        if let Some(compression) = self.mono_compression {
            settings.mono.compression = compression;
        }

        if self.no_downsample {
            settings.color.downsample = false;
            settings.gray.downsample = false;
            settings.mono.downsample = false;
        } else {
            if self.no_color_downsample {
                settings.color.downsample = false;
            }
            if self.no_gray_downsample {
                settings.gray.downsample = false;
            }
            if self.no_mono_downsample {
                settings.mono.downsample = false;
            }
        }

        settings.merge_fragments = !self.no_merge_fragments;
        settings.discard_thumbnails = !self.keep_thumbnails;
        settings.discard_alternate_images = !self.keep_alternate_images;
        settings.discard_tags = self.discard_tags;
        settings.discard_print_settings = self.discard_print_settings;
        settings.flate_unencoded = !self.no_flate;
        settings.lzw_to_flate = !self.no_lzw_to_flate;
        settings.object_streams = !self.no_object_streams;
        settings.remove_invalid_links = !self.keep_invalid_links;
        settings.remove_unreferenced_dests = !self.keep_unreferenced_dests;
        Ok(settings)
    }
}

fn apply_images(
    base: ImageSettings,
    compression: Option<Compression>,
    quality: Option<u8>,
    default_quality: u8,
) -> ImageSettings {
    let quality = quality.map(|q| q.clamp(1, 100));
    let mut settings = base;
    if let Some(compression) = compression {
        settings.compression = match compression {
            Compression::Jpeg(_) => Compression::Jpeg(quality.unwrap_or(default_quality)),
            other => other,
        };
    } else if let (Some(q), Compression::Jpeg(_)) = (quality, base.compression) {
        settings.compression = Compression::Jpeg(q);
    }
    settings
}

fn clamp_ppi(value: f64) -> f64 {
    value.clamp(9.0, 2400.0)
}

/// The result of compressing one document.
pub struct CompressOutcome {
    pub bytes: Vec<u8>,
    pub report: Report,
    pub merged_resources: usize,
    /// The optimizer did not make the file smaller, so the output is the input's bytes.
    pub reused_input: bool,
    pub object_streams: bool,
    pub strip: Option<StripOutcome>,
}

/// Open, optionally strip signatures, optimize, deduplicate resources and write a full copy.
pub fn run(
    input: Arc<Vec<u8>>,
    password: Option<&str>,
    settings: &Settings,
    strip_signatures: bool,
) -> Result<CompressOutcome> {
    let input_len = input.len();
    let mut doc = Document::open_with_password(input.clone(), password).map_err(open_error)?;
    check_permissions(&doc, strip_signatures)?;

    let signatures_before = signatures::inspect(&doc, true);
    if (signatures_before.signed() || signatures_before.dictionaries > 0) && !strip_signatures {
        bail!(
            "the PDF is digitally signed ({} signed signature field(s), {} signature dictionary(ies){}); \
             PdfCraft refuses to optimize signed documents. Use `pdfskim clean`, or add --strip-signatures to remove them first \
             (this invalidates the signatures)",
            signatures_before.signed_fields,
            signatures_before.dictionaries,
            if signatures_before.perms {
                ", plus /Perms certification"
            } else {
                ""
            }
        );
    }

    let strip = if strip_signatures {
        Some(signatures::strip(&mut doc)?)
    } else {
        None
    };

    let report =
        optimize(&mut doc, settings).map_err(|e| anyhow::anyhow!("optimization failed: {e}"))?;
    let candidates: Vec<ObjRef> = doc
        .object_numbers()
        .into_iter()
        .map(|num| ObjRef::new(num, doc.generation(num)))
        .collect();
    let merged_resources = dedupe_resources(&mut doc, &candidates, false);

    let options = SaveOptions {
        mod_date: Some(pdf_date(unix_now())),
        object_streams: settings.effective_object_streams(),
        ..SaveOptions::default()
    };
    let optimized = write_full(&doc, &options).with_context(|| "cannot write the optimized PDF")?;

    let stripped = strip.as_ref().is_some_and(|s| s.changed);
    if strip.is_some() {
        // A full rewrite is the only way to drop the revisions that hold the signed bytes; prove
        // that nothing signature-shaped survived before reporting success.
        let check = Document::open_with_password(Arc::new(optimized.clone()), password)
            .map_err(open_error)?;
        let after = signatures::inspect(&check, true);
        if after.any() {
            bail!(
                "signature removal did not complete: {} field(s), {} signature dictionary(ies), /Perms: {}, /DSS: {} remain",
                after.fields,
                after.dictionaries,
                after.perms,
                after.dss
            );
        }
    }

    let (bytes, reused_input) = if !stripped && optimized.len() >= input_len {
        ((*input).clone(), true)
    } else {
        (optimized, false)
    };

    Ok(CompressOutcome {
        bytes,
        report,
        merged_resources,
        reused_input,
        object_streams: settings.effective_object_streams(),
        strip,
    })
}

fn check_permissions(doc: &Document, strip: bool) -> Result<()> {
    let Some(permissions) = doc.permissions() else {
        return Ok(());
    };
    if permissions.owner {
        return Ok(());
    }
    if strip {
        bail!("removing signatures from an encrypted PDF requires its owner password");
    }
    if !permissions.modify() {
        bail!("the document's permissions do not allow changes (open it with the owner password)");
    }
    Ok(())
}

pub(crate) fn open_error(error: CosError) -> anyhow::Error {
    match error {
        CosError::NeedsPassword => anyhow::anyhow!(
            "the PDF is encrypted and needs a password (pass --password or set PDFSKIM_PASSWORD)"
        ),
        CosError::WrongPassword => anyhow::anyhow!("the password is incorrect"),
        other => anyhow::anyhow!("cannot open the PDF: {other}"),
    }
}

pub(crate) fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
