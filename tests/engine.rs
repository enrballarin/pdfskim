//! Engine-level tests: compression settings, signed-document refusal and signature stripping.

mod common;

use std::sync::Arc;

use pdfcraft_cos::{Document, SaveOptions, write_full};
use pdfcraft_optimize::Preset;
use pdfskim::compress::{self, OptimizeOptions};
use pdfskim::signatures;

fn open(bytes: &[u8]) -> Document {
    Document::open(Arc::new(bytes.to_vec())).expect("a readable PDF")
}

fn standard() -> pdfcraft_optimize::Settings {
    OptimizeOptions {
        preset: Some(Preset::Standard),
        ..Default::default()
    }
    .settings()
    .expect("settings")
}

#[test]
fn compress_downsamples_a_high_resolution_image() {
    let input = common::image_pdf();
    let outcome =
        compress::run(Arc::new(input.clone()), None, &standard(), false).expect("compression");
    assert!(
        outcome.report.images_resampled >= 1,
        "expected one resampled image: {:?}",
        outcome.report
    );
    assert!(
        outcome.bytes.len() < input.len(),
        "{} -> {}",
        input.len(),
        outcome.bytes.len()
    );
    assert!(!outcome.reused_input);
    let reopened = open(&outcome.bytes);
    assert_eq!(
        pdfcraft_annot::page_refs(&reopened).expect("pages").len(),
        1
    );
}

#[test]
fn compress_refuses_a_signed_pdf() {
    let input = common::signed_pdf();
    let before = signatures::inspect(&open(&input), true);
    assert_eq!(before.fields, 1);
    assert!(before.signed());
    assert!(before.dictionaries >= 1);
    assert!(before.perms && before.dss);

    let error = match compress::run(Arc::new(input), None, &standard(), false) {
        Ok(_) => panic!("a signed PDF must be refused"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("digitally signed"), "{error}");
}

#[test]
fn strip_removes_signature_fields_perms_and_validation_data() {
    let input = common::signed_pdf();
    let mut doc = open(&input);
    let outcome = signatures::strip(&mut doc).expect("strip");
    assert!(outcome.changed);
    assert_eq!(outcome.report.fields, 1);
    assert_eq!(outcome.report.signed_fields, 1);
    assert_eq!(outcome.annotations_removed, 1);

    let bytes = write_full(
        &doc,
        &SaveOptions {
            object_streams: true,
            ..SaveOptions::default()
        },
    )
    .expect("write");
    assert!(
        !contains(&bytes, b"ByteRange"),
        "the signature dictionary survived"
    );
    assert!(
        !contains(&bytes, b"Adobe.PPKLite"),
        "the signature handler survived"
    );
    assert!(
        !contains(&bytes, b"Ada Lovelace"),
        "the signer name survived"
    );

    let reopened = open(&bytes);
    let after = signatures::inspect(&reopened, true);
    assert_eq!(after.fields, 0);
    assert_eq!(after.dictionaries, 0);
    assert!(!after.perms);
    assert!(!after.dss);
    assert_eq!(
        pdfcraft_annot::page_refs(&reopened).expect("pages").len(),
        1
    );
}

#[test]
fn strip_drops_the_older_revisions_that_hold_the_signed_bytes() {
    let base = common::signed_pdf();
    let mut doc = open(&base);
    let root = doc.root().expect("catalog");
    doc.update_dict(root, |catalog| {
        catalog.set(
            b"Lang".to_vec(),
            pdfcraft_cos::Object::String(pdfcraft_cos::PdfString::text("es")),
        );
    })
    .expect("edit");
    let two_revisions =
        pdfcraft_cos::write_incremental(&doc, &SaveOptions::default()).expect("incremental save");
    assert!(
        two_revisions.len() > base.len(),
        "the second revision was appended"
    );

    let mut doc = Document::open(Arc::new(two_revisions.clone())).expect("reopen");
    assert!(signatures::inspect(&doc, true).signed());
    signatures::strip(&mut doc).expect("strip");
    let cleaned = write_full(
        &doc,
        &SaveOptions {
            object_streams: true,
            ..SaveOptions::default()
        },
    )
    .expect("full save");
    assert!(
        !contains(&cleaned, b"ByteRange"),
        "the signed revision is not carried over"
    );
    assert!(
        cleaned.len() < two_revisions.len(),
        "the old revision's bytes were dropped"
    );
    assert!(!signatures::inspect(&open(&cleaned), true).any());
}

#[test]
fn strip_keeps_other_form_fields_and_their_widgets() {
    let input = common::signed_form_pdf();
    let mut doc = open(&input);
    let outcome = signatures::strip(&mut doc).expect("strip");
    assert!(outcome.changed);
    assert_eq!(outcome.report.fields, 2, "both signature fields");
    assert_eq!(outcome.annotations_removed, 2, "both signature widgets");

    let bytes = write_full(
        &doc,
        &SaveOptions {
            object_streams: true,
            ..SaveOptions::default()
        },
    )
    .expect("write");
    assert!(!contains(&bytes, b"ByteRange"));
    let reopened = open(&bytes);
    assert!(
        !signatures::inspect(&reopened, true).any(),
        "no signature remains"
    );

    let root = reopened.root().expect("catalog");
    let acro = dict_of(
        &reopened,
        reopened
            .get(root)
            .as_dict()
            .unwrap()
            .get(b"AcroForm")
            .expect("AcroForm"),
    )
    .expect("AcroForm dict");
    let fields = array_of(&reopened, acro.get(b"Fields").expect("Fields")).expect("Fields array");
    assert_eq!(fields.len(), 1, "the text field survives");
    let text = dict_of(&reopened, &fields[0]).expect("text field");
    assert_eq!(text.name(b"FT"), Some(&b"Tx"[..]));
    assert_eq!(
        text.get(b"V")
            .and_then(|v| v.as_string())
            .map(|s| s.to_text()),
        Some(String::from("Ada"))
    );
    let co = array_of(&reopened, acro.get(b"CO").expect("CO")).expect("CO array");
    assert_eq!(
        co.len(),
        1,
        "only the text field is left in the calculation order"
    );

    let pages = pdfcraft_annot::page_refs(&reopened).expect("pages");
    let annots = reopened
        .get(pages[0])
        .as_dict()
        .and_then(|d| d.get(b"Annots"))
        .and_then(|a| array_of(&reopened, a))
        .expect("Annots");
    assert_eq!(annots.len(), 1, "only the text widget is left on the page");
}

#[test]
fn strip_is_a_no_op_without_signatures() {
    let mut doc = open(&common::text_pdf());
    let outcome = signatures::strip(&mut doc).expect("strip");
    assert!(!outcome.changed, "nothing to remove");
}

#[test]
fn clean_strips_then_compresses() {
    let input = common::signed_image_pdf();
    let outcome = compress::run(Arc::new(input.clone()), None, &standard(), true).expect("clean");
    assert!(outcome.strip.as_ref().is_some_and(|s| s.changed));
    assert!(!outcome.reused_input);
    assert!(outcome.bytes.len() < input.len());
    let reopened = open(&outcome.bytes);
    assert!(!signatures::inspect(&reopened, true).any());
    assert!(outcome.report.images_resampled >= 1);
}

#[test]
fn encrypted_signed_pdf_requires_the_owner_password_and_stays_encrypted() {
    let bytes = common::encrypted_signed_pdf();
    let error = match compress::run(Arc::new(bytes.clone()), None, &standard(), true) {
        Ok(_) => panic!("an encrypted PDF must need a password"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("needs a password"), "{error}");

    let error = match compress::run(Arc::new(bytes.clone()), Some("user"), &standard(), true) {
        Ok(_) => panic!("stripping must need the owner password"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("owner password"), "{error}");

    let outcome = compress::run(Arc::new(bytes), Some("owner"), &standard(), true)
        .expect("owner can strip and compress");
    let reopened = Document::open_with_password(Arc::new(outcome.bytes.clone()), Some("owner"))
        .expect("output opens");
    assert!(reopened.security().is_some(), "the output stays encrypted");
    assert!(!signatures::inspect(&reopened, true).any());
    assert!(
        Document::open_with_password(Arc::new(outcome.bytes), Some("user")).is_ok(),
        "the user password still opens it"
    );
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn dict_of(doc: &Document, object: &pdfcraft_cos::Object) -> Option<pdfcraft_cos::Dict> {
    match &*doc.resolve(object) {
        pdfcraft_cos::Object::Dict(dict) => Some(dict.clone()),
        _ => None,
    }
}

fn array_of(doc: &Document, object: &pdfcraft_cos::Object) -> Option<Vec<pdfcraft_cos::Object>> {
    match &*doc.resolve(object) {
        pdfcraft_cos::Object::Array(array) => Some(array.clone()),
        _ => None,
    }
}
