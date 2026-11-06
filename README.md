# pdfskim

A Rust console CLI to **compress PDFs** and **remove digital signatures**, built on the
[PdfCraft](https://github.com/storytold/pdfcraft) engine. It works on a single file or in
batches (several files and whole folders), with an optional JSON report.

```console
$ pdfskim compress report.pdf
$ pdfskim compress scans/ --recursive --out-dir compressed --quality 45 --ppi 120
$ pdfskim strip signed.pdf
$ pdfskim clean signed.pdf -o signed-clean.pdf
```

---

## 1. What it does exactly (and what it doesn't)

| Feature | Implementation | Notes |
|---|---|---|
| Compress | `pdfcraft-optimize`, the same optimizer PdfCraft uses in *Reduce File Size* and *PDF Optimizer* | Downsamples and recompresses images, merges fragments, deduplicates resources, compresses streams and writes object streams |
| Strip signatures | pdfskim's own code on top of `pdfcraft-cos` | PdfCraft does **not** remove signatures: its optimizer **rejects** signed PDFs and "clear signature fields" is listed as *planned* in its Acrobat parity list. pdfskim adds that part |
| Batches | pdfskim | Sequential, with the same limits as PdfCraft's `batch` command |
| Report | pdfskim | Human-readable text or JSON on stdout |

**What it does NOT do:**

- It does not convert Office/images to PDF or run OCR (PdfCraft does have `convert` and `ocr` in its `batch`).
- It does not run *Remove Hidden Information*: it does not erase metadata, attachments, comments,
  bookmarks or JavaScript. It only discards what is requested with `--discard-tags` /
  `--discard-print-settings`, plus thumbnails/alternate images by default.
- It does not embed or remove passwords; it preserves the original encryption.
- It does not remove "flattened" signatures baked into the page content, nor signatures inside XFA (see §8).
- It does not parallelize: it processes one PDF at a time (each file is loaded entirely into memory).

---

## 2. Building

Requirements: Rust (the channel is set in `rust-toolchain.toml`; the minimum is 1.95, as declared in
`Cargo.toml`) and the PdfCraft checkout in `_vendor/pdfcraft` (already included in this project,
pinned to commit `d7ce875dd692109789bcdad1b683ffc5a2d07dc7`).

```sh
cargo build --release
# binary: target/release/pdfskim (pdfskim.exe on Windows)
```

If you start from another machine, clone PdfCraft at the same path and commit:

```sh
git clone https://github.com/storytold/pdfcraft _vendor/pdfcraft
git -C _vendor/pdfcraft checkout d7ce875dd692109789bcdad1b683ffc5a2d07dc7
cargo build --release
```

`Cargo.toml` applies the same `hayro-syntax` patches that PdfCraft keeps in
`_vendor/pdfcraft/vendor/`; without that checkout the project does not build.

---

## 3. Commands

```
pdfskim <COMMAND> [OPTIONS] <INPUT>...
```

| Command | What it does | Output suffix |
|---|---|---|
| `compress` | Compresses. If the PDF is signed, it **fails** and explains how to proceed | `.compressed.pdf` |
| `strip` | Removes digital signatures and certification, and rewrites the PDF | `.unsigned.pdf` |
| `clean` | Removes signatures first, then compresses | `.clean.pdf` |
| `info` | Shows size, version, pages, encryption and signatures, without modifying anything | — |

### Examples

```sh
# One file: writes report.compressed.pdf next to it
pdfskim compress report.pdf

# Choose output and quality
pdfskim compress report.pdf -o report-web.pdf --quality 45 --ppi 120

# Batch: a folder and its subfolders, preserving the structure
pdfskim compress scans/ --recursive --out-dir compressed

# Wildcards work on Windows too (cmd.exe does not expand them)
pdfskim compress *.pdf --out-dir compressed

# Several loose files to the same destination
pdfskim compress a.pdf b.pdf c.pdf --out-dir output --json

# Remove signatures (invalidates them) and then compress
pdfskim clean contract.pdf -o contract-clean.pdf
# Equivalent to:
pdfskim compress contract.pdf --strip-signatures -o contract-clean.pdf

# See which signatures and which encryption it has
pdfskim info contract.pdf

# Simulate without writing anything
pdfskim compress scans/ -r --out-dir compressed --dry-run
```

---

Wildcards are expanded by pdfskim itself, so they also work in `cmd.exe`, which passes `*.pdf`
through untouched:

| Pattern | Matches |
|---|---|
| `*.pdf` | every PDF in the current folder |
| `scans/*.pdf` | every PDF directly inside `scans` |
| `**/*.pdf` | every PDF in this folder and in every folder below it |
| `chapter?.pdf` | `chapter1.pdf`, `chapter2.pdf`, … |
| `[ab]*.pdf` | names starting with `a` or `b` |

`*`, `?`, `[abc]`, `[a-z]` and `[!a-z]` match inside one path component; `**` matches any number
of folders. A pattern that matches nothing is an error, so a typo never turns into an empty batch.

## 4. Common options

Accepted by `compress`, `strip` and `clean` (in `info` only `--password` and `--json`).

| Switch | Value | Default | Description |
|---|---|---|---|
| `-o`, `--out <FILE>` | path | — | Output for a **single** input file. Cannot be combined with `--out-dir` |
| `--out-dir <DIR>` | folder | — | Root output folder. Required if any input is a folder. Preserves subfolders |
| `-r`, `--recursive` | flag | no | Recurses into subfolders. Without it, only the direct children of each folder are read |
| `--overwrite` | flag | no | Replaces existing output files. Without it, automatic names are numbered (` (2)`, ` (3)`…) and `-o` fails if the destination exists |
| `--password <PASSWORD>` | text | — | Password of the encrypted PDF. The `PDFSKIM_PASSWORD` environment variable also works |
| `--max-input-mib <MIB>` | 1–4096 | `512` | Maximum size per input PDF, in MiB. Above 512 it warns: that is the limit PdfCraft tests |
| `--json` | flag | no | A single JSON report on stdout; progress goes to stderr |
| `-q`, `--quiet` | flag | no | Does not print per-file progress |
| `--dry-run` | flag | no | Does all the work (including compression) but writes no output |
| `--stop-on-error` | flag | no | Stops at the first failed file; the rest are left as *skipped* |

### Output names and collisions

- A single file without `--out`/`--out-dir` → `<name>.compressed.pdf` (or `.unsigned` / `.clean`) in
  its own folder.
- Several files or `--out-dir` → inside `--out-dir`, preserving each input folder's relative path.
- An input is never overwritten: if the computed output is the input file itself, it is rejected.
- If two inputs produce the same name, the second becomes `name (2).pdf`, etc. This also applies
  with `--overwrite` within the same run.

---

## 5. Compression options

Only in `compress` and `clean`. Defaults depend on the preset.

| Switch | Value | Standard | Mobile | Description |
|---|---|---|---|---|
| `--preset <P>` | `standard`, `mobile` | `standard` | — | `standard` = *Reduce File Size*; `mobile` = 96 ppi, JPEG 80 |
| `--compatibility <V>` | `retain`, `1.4`, `1.5`, `1.6`, `1.7`, `2.0` | `retain` | `retain` | Header version. `1.4` disables object streams. Partial compatibility only: it does not convert newer content |
| `--quality <N>` | 1–100 | 60 | 80 | JPEG quality for color and grayscale images. Ignored if compression is `zip`/`retain` |
| `--ppi <N>` | 9–2400 | 150 | 96 | Target resolution for color and grayscale |
| `--above-ppi <N>` | 9–2400 | 1.5 × `--ppi` (225 / 144) | same | Downsampling threshold for color and grayscale. Forced to `>= --ppi` |
| `--mono-ppi <N>` | 9–2400 | `max(--ppi, 300)` = 300 | same | Target resolution for black and white |
| `--mono-above-ppi <N>` | 9–2400 | 1.5 × `--mono-ppi` (450) | same | Downsampling threshold for black and white |
| `--color-compression <C>` | `jpeg`, `zip`, `retain` | `jpeg` | `jpeg` | Codec for color images |
| `--gray-compression <C>` | `jpeg`, `zip`, `retain` | `jpeg` | `jpeg` | Codec for grayscale images |
| `--mono-compression <C>` | `ccitt-g4`, `jbig2`, `retain` | `ccitt-g4` | `ccitt-g4` | Lossless codec for 1-bit images |
| `--no-downsample` | flag | — | — | No downsampling at all (equivalent to the three `--no-*-downsample`) |
| `--no-color-downsample` | flag | — | — | Keeps the resolution of color images |
| `--no-gray-downsample` | flag | — | — | Keeps the resolution of grayscale images |
| `--no-mono-downsample` | flag | — | — | Keeps the resolution of 1-bit images |
| `--no-merge-fragments` | flag | — | — | Does not merge images drawn as one-pixel strips |
| `--keep-thumbnails` | flag | — | — | Keeps page thumbnails (discarded by default) |
| `--keep-alternate-images` | flag | — | — | Keeps alternate images (discarded by default) |
| `--discard-tags` | flag | — | — | Discards the structure tree (accessibility tags) |
| `--discard-print-settings` | flag | — | — | Discards print settings (scale, duplex, range, copies) |
| `--no-flate` | flag | — | — | Does not Flate-compress streams that have no filter |
| `--no-lzw-to-flate` | flag | — | — | Leaves LZW streams as LZW |
| `--no-object-streams` | flag | — | — | Writes a classic xref table instead of object streams (PDF 1.5+) |
| `--keep-invalid-links` | flag | — | — | Keeps links and bookmark destinations that point nowhere |
| `--keep-unreferenced-dests` | flag | — | — | Keeps unreferenced named destinations |
| `--strip-signatures` | flag | — | — | In `compress`: removes signatures first (equivalent to `clean`). Without this flag, a signed PDF is rejected |

### How `--preset`, `--ppi` and `--quality` combine

1. Start from the preset values (`standard` or `mobile`).
2. `--ppi` sets the color and grayscale target; if you don't pass `--above-ppi`, the threshold becomes
   `1.5 × ppi`. For black and white, the target becomes `max(ppi, 300)` unless you pass `--mono-ppi`.
3. `--above-ppi` and `--mono-above-ppi` override the calculation above; they never end up below the
   target.
4. `--quality` changes the JPEG quality for color and grayscale. It only has an effect with `jpeg`
   compression.

---

## 6. Output and exit codes

### Text

```
pdfskim compress: 2 PDF(s)
[1/2] compress  C:\docs\a.pdf -> C:\docs\a.compressed.pdf (12.4 MiB (-71.2%))
[2/2] compress  C:\docs\b.pdf -> C:\docs\b.compressed.pdf (no reduction, kept original)
Done: 2 completed, 0 failed, 0 skipped; 24.8 MiB -> 8.1 MiB (16.7 MiB saved) in 3.4s
```

### JSON (`--json`)

```json
{
  "tool": "pdfskim",
  "version": "0.1.0",
  "command": "clean",
  "dry_run": false,
  "summary": {
    "selected": 2,
    "completed": 2,
    "failed": 0,
    "skipped": 0,
    "bytes_before": 26000000,
    "bytes_after": 8500000,
    "saved_bytes": 17500000,
    "saved_percent": 67.3,
    "signatures_removed": 0,
    "elapsed_seconds": 3.4
  },
  "files": [
    {
      "input": "C:\\docs\\a.pdf",
      "output": "C:\\docs\\a.compressed.pdf",
      "status": "ok",
      "bytes_before": 13000000,
      "bytes_after": 4200000,
      "saved_bytes": 8800000,
      "saved_percent": 67.7,
      "reused_input": false,
      "optimize": {
        "images": 3,
        "images_resampled": 2,
        "images_recompressed": 1,
        "images_refused": 0,
        "merged_images": 0,
        "fragments_merged": 0,
        "merged_resources": 4,
        "thumbnails": 1,
        "alternate_images": 0,
        "tags_removed": false,
        "print_settings": 0,
        "streams_compressed": 12,
        "lzw_streams": 0,
        "invalid_links": 0,
        "invalid_bookmarks": 0,
        "unreferenced_dests": 0,
        "unused_xobjects": 2,
        "object_streams": true
      },
      "signatures": {
        "fields": 1,
        "signed_fields": 1,
        "dictionaries": 1,
        "perms": true,
        "dss": false,
        "xfa": false,
        "annotations_removed": 1,
        "names": ["Signature1"]
      }
    }
  ],
  "skipped": [
    { "path": "C:\\docs\\notes.txt", "reason": "not a PDF file" }
  ]
}
```

`optimize` only appears in `compress`/`clean`; `signatures` only when there are signatures (or in
`strip`). `saved_percent` is 0 if nothing was reduced.

### Exit codes

| Code | Meaning |
|---|---|
| `0` | All good (there may be skipped files) |
| `1` | At least one file failed, or a global error (nonexistent path, incompatible arguments) |
| `2` | CLI usage error (emitted by `clap`) |
| `3` | No PDF was found, or `strip` found no signatures in any of them |

---

## 7. Limits

These are the program's real limits. Some come from PdfCraft and others are pdfskim decisions; they
are listed here so there are no surprises.

### 7.1 Size and batches

| Limit | Value | Where |
|---|---|---|
| Maximum size per PDF | **512 MiB** by default (`--max-input-mib`, 1–4096) | Before reading the file |
| Files selected per batch | **1,000** | Discovery |
| Folder entries visited | **10,000** | Discovery |
| Folder depth | **64 levels** | Discovery |
| Concurrent processing | **1 file at a time** | The whole program |

Exceeding 1,000/10,000/64 aborts the batch with an error. Symlinks, Windows *reparse points* and
special files are skipped when walking folders and are rejected as an explicit input. A file that is
not `.pdf` is recorded as *skipped*.

`info` has its own fixed cap of 512 MiB per file.

**Memory:** each PDF is loaded entirely (`Arc<Vec<u8>>`) and the output is built in memory as well.
The peak is around 2–3 times the file size. Going above 512 MiB is possible but falls outside what
PdfCraft tests.

### 7.2 Optimizer

| Limit | Value | Effect |
|---|---|---|
| `--ppi` / `--mono-ppi` | 9–2400 | Out of range: argument error |
| `--quality` | 1–100 | Out of range: argument error |
| Decoded stream | **1 GiB** (`MAX_DECODED`) | A larger stream fails; this is the defense against decompression bombs |
| Page content | **256 MiB** and **50,000** Form XObjects | The page stops being walked |
| `/JBIG2Globals` | **32 MiB** | Larger JBIG2 dictionary: not decoded |
| Image decoding work | ≈ **1,000 scanned pages at 300 dpi** + 8,192 steps per byte of the file | Images that exceed the budget **are left as they are** and counted in `images_refused` |
| JBIG2 | Up to **16 images** share a dictionary; images up to **64 Mi pixels** | Beyond that, Flate/CCITT is used or they are left as they are |
| Images | Replaced **only if the result is smaller** | `images_resampled` / `images_recompressed` account for it |
| Whole file | If the result is not smaller, the output is a **byte-for-byte copy** of the input | `reused_input: true` |

The optimizer does **not** touch text, fonts or vector graphics: if a PDF is text only, the reduction
may be minimal (object streams, cleanup). There is no "images only" or "lossless" mode: JPEG and
downsampling are **lossy**. `zip` and CCITT G4/JBIG2 are lossless, but downsampling is still lossy.

Limitations inherited from PdfCraft: it does not recompress JPEG 2000, does not unembed fonts, does
not flatten transparency, does not convert content to older versions, and `1.4` compatibility is
partial (it does not detect newer content).

### 7.3 Signatures and encryption

| Case | Behavior |
|---|---|
| Signed PDF | `compress` **fails** and suggests `clean` or `--strip-signatures` |
| `strip` / `clean` | Removes `/FT /Sig` fields (signed or empty), their widgets, `/Perms`, `/DSS`, `/SigFlags` and `/CO` entries; rewrites in **full mode** with a new `/ID`, so old revisions (the signed bytes) disappear |
| Flattened signature | **Cannot be removed**: it is pixels in the page content |
| XFA | A warning is issued (`xfa: true`); signatures inside the XML are **not** inspected or erased |
| "Fill & Sign" signature | It is an annotation, not a `/Sig` field; `strip` does not remove it (it is handled as a comment/annotation in PdfCraft) |
| Encrypted PDF | `--password` (or `PDFSKIM_PASSWORD`). For `strip`/`clean` the **owner password** is required; the user password is rejected. The output stays encrypted |
| Author permissions | If the PDF is encrypted and permissions do not allow changes, `compress` fails asking for the owner password |
| Certification | Removing `/Perms` also removes the certification signature and its restrictions (DocMDP/UR3) |

**Warning:** removing signatures is destructive and irreversible. It invalidates existing signatures
and any later validation. Always work on a copy.

---

## 8. What exactly `strip` deletes

1. Walks `/AcroForm /Fields` (including nested fields and `/Kids`) and locates fields with
   `/FT /Sig` (inherited or their own).
2. Removes those fields from the tree, their widgets from the pages' `/Annots`, their entries in
   `/CO` (calculation order) and `/SigFlags`.
3. Removes `/Perms` (DocMDP/UR3) and `/DSS` (validation data) from the catalog.
4. If `/AcroForm` is left with no fields, it removes it.
5. Does a **full save** with a new identifier: only reachable objects are written, so signature
   dictionaries and old revisions do not make it into the file.
6. Reopens the result and verifies that no `/Sig` field, signature dictionary, `/Perms` or `/DSS`
   remains. If anything is left, it fails instead of reporting a false success.

`strip` preserves the header: it uses object streams only if the PDF was already 1.5 or newer, so a
1.4 stays 1.4. In `compress`/`clean`, the optimizer writes object streams by default and can raise a
1.4 to 1.5; `--no-object-streams` keeps the classic xref table.

Everything else is preserved: non-signature form fields, values, comments, bookmarks, attachments,
metadata and page content.

---

## 9. Differences from `pdfcraft-cli`

| | `pdfcraft-cli batch --steps optimize` | `pdfskim` |
|---|---|---|
| Compression | Yes, with `--ppi` and `--quality` | Yes, with all optimizer options |
| Signatures | Rejected; no removal (planned) | `strip` / `clean` remove them |
| Batch | Yes | Yes, with the same limits |
| JSON | Its own report | Its own report with image statistics |
| Encryption | Not supported in `batch` | `--password`, owner password for `strip` |
| PdfCraft extras | `convert`, `ocr`, `--stop-on-error` | No |

If you already use PdfCraft, its `pdfcraft-cli batch` remains the most direct option for compressing
files without signatures. pdfskim adds signature removal and an interface designed to fit into
scripts.

---

## 10. License and attribution

pdfskim is distributed under **MIT OR Apache-2.0**, same as PdfCraft. The PdfCraft code
(`_vendor/pdfcraft`, MIT OR Apache-2.0) keeps its notices and licenses. The vendored, patched `hayro`
and `lopdf` crates under `_vendor/pdfcraft/vendor/` keep their own, listed in
`_vendor/pdfcraft/vendor/README.md` (the licence table) and in each crate's `LICENSE-*` files. The
third-party notices those licences require are reproduced in [NOTICE](NOTICE).

PdfCraft's ArtCraft brand assets (`docs/brand/`) are not part of this distribution.

PdfCraft is a project by the ArtCraft Team and its contributors. This CLI is not an official product
of PdfCraft or ArtCraft.
