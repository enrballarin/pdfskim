//! Synthetic PDFs for the integration tests. They are built with PdfCraft's own COS layer,
//! so the tests exercise the same object model the tool uses.

#![allow(dead_code)]

use std::sync::Arc;

use pdfcraft_cos::{
    Algorithm, Dict, Document, NewEncryption, ObjRef, Object, PdfString, SaveOptions, Stream,
    write_full,
};

/// A one-page text PDF, 612 x 792 points.
pub fn text_pdf() -> Vec<u8> {
    let mut doc = Document::new_empty();
    let (_, page) = blank_page(
        &mut doc,
        612,
        792,
        b"BT /F1 24 Tf 72 720 Td (Pdfskim test) Tj ET",
    );
    finish(doc, page)
}

/// A one-page PDF whose only content is a large RGB image, so the optimizer has something to
/// downsample: a 144 x 144 pt page (2 x 2 in) with a 1440 x 1440 image is drawn at 720 ppi.
pub fn image_pdf() -> Vec<u8> {
    let mut doc = Document::new_empty();
    let mut image = Dict::new();
    image.set(b"Type".to_vec(), Object::name("XObject"));
    image.set(b"Subtype".to_vec(), Object::name("Image"));
    image.set(b"Width".to_vec(), Object::Int(1440));
    image.set(b"Height".to_vec(), Object::Int(1440));
    image.set(b"ColorSpace".to_vec(), Object::name("DeviceRGB"));
    image.set(b"BitsPerComponent".to_vec(), Object::Int(8));
    let stream = Stream::flate(image, &noise(1440 * 1440 * 3));
    let image = doc.add(Object::Stream(stream));

    let mut xobjects = Dict::new();
    xobjects.set(b"Im1".to_vec(), Object::Ref(image));
    let mut resources = Dict::new();
    resources.set(b"XObject".to_vec(), Object::Dict(xobjects));
    let resources = doc.add(Object::Dict(resources));
    let content = doc.add(Object::Stream(Stream::flate(
        Dict::new(),
        b"q 144 0 0 144 0 0 cm /Im1 Do Q",
    )));

    let pages = pages_ref(&doc);
    let mut page = Dict::new();
    page.set(b"Type".to_vec(), Object::name("Page"));
    page.set(b"Parent".to_vec(), Object::Ref(pages));
    page.set(
        b"MediaBox".to_vec(),
        Object::Array(vec![0.into(), 0.into(), 144.into(), 144.into()]),
    );
    page.set(b"Resources".to_vec(), Object::Ref(resources));
    page.set(b"Contents".to_vec(), Object::Ref(content));
    let page = doc.add(Object::Dict(page));
    doc.update_dict(pages, |dict| {
        dict.set(b"Kids".to_vec(), Object::Array(vec![Object::Ref(page)]));
        dict.set(b"Count".to_vec(), Object::Int(1));
    })
    .expect("fixture");
    finish(doc, page)
}

/// A signed-looking PDF: an AcroForm signature field (merged with its widget), `/V` pointing to
/// a signature dictionary, `/Perms` with a DocMDP reference and `/DSS` validation data.
pub fn signed_pdf() -> Vec<u8> {
    let mut doc = Document::new_empty();
    let (_, page) = blank_page(&mut doc, 612, 792, b"BT /F1 24 Tf 72 720 Td (Signed) Tj ET");

    let mut sig = Dict::new();
    sig.set(b"Type".to_vec(), Object::name("Sig"));
    sig.set(b"Filter".to_vec(), Object::name("Adobe.PPKLite"));
    sig.set(b"SubFilter".to_vec(), Object::name("adbe.pkcs7.detached"));
    sig.set(
        b"ByteRange".to_vec(),
        Object::Array(vec![0.into(), 100.into(), 200.into(), 300.into()]),
    );
    sig.set(
        b"Contents".to_vec(),
        Object::String(PdfString::literal(vec![0x30, 0x82, 0xDE, 0xAD])),
    );
    sig.set(
        b"Name".to_vec(),
        Object::String(PdfString::text("Ada Lovelace")),
    );
    let sig = doc.add(Object::Dict(sig));

    let mut widget = Dict::new();
    widget.set(b"Type".to_vec(), Object::name("Annot"));
    widget.set(b"Subtype".to_vec(), Object::name("Widget"));
    widget.set(b"FT".to_vec(), Object::name("Sig"));
    widget.set(b"T".to_vec(), Object::String(PdfString::text("Signature1")));
    widget.set(b"F".to_vec(), Object::Int(4));
    widget.set(
        b"Rect".to_vec(),
        Object::Array(vec![72.into(), 600.into(), 300.into(), 680.into()]),
    );
    widget.set(b"P".to_vec(), Object::Ref(page));
    widget.set(b"V".to_vec(), Object::Ref(sig));
    let widget = doc.add(Object::Dict(widget));
    doc.update_dict(page, |dict| {
        dict.set(b"Annots".to_vec(), Object::Array(vec![Object::Ref(widget)]));
    })
    .expect("fixture");

    let mut acro = Dict::new();
    acro.set(b"Fields".to_vec(), Object::Array(vec![Object::Ref(widget)]));
    acro.set(b"SigFlags".to_vec(), Object::Int(3));
    let acro = doc.add(Object::Dict(acro));

    let mut mdp = Dict::new();
    mdp.set(b"Type".to_vec(), Object::name("SigRef"));
    mdp.set(b"TransformMethod".to_vec(), Object::name("DocMDP"));
    mdp.set(b"DigestMethod".to_vec(), Object::name("SHA256"));
    mdp.set(b"V".to_vec(), Object::Ref(sig));

    let mut perms = Dict::new();
    perms.set(b"DocMDP".to_vec(), Object::Dict(mdp));
    let root = doc.root().expect("catalog");
    doc.update_dict(root, |catalog| {
        catalog.set(b"AcroForm".to_vec(), Object::Ref(acro));
        catalog.set(b"Perms".to_vec(), Object::Dict(perms));
        catalog.set(b"DSS".to_vec(), Object::Dict(Dict::new()));
    })
    .expect("fixture");
    finish(doc, page)
}

/// A signed PDF that also carries an image, for the clean-then-compress test.
pub fn signed_image_pdf() -> Vec<u8> {
    let image_doc = image_pdf();
    let image = Document::open(Arc::new(image_doc)).expect("image fixture");
    let pages = pages_ref(&image);
    let page = image
        .get(pages)
        .as_dict()
        .and_then(|d| d.get(b"Kids"))
        .and_then(|k| k.as_array())
        .and_then(|k| k.first())
        .and_then(|p| p.as_ref())
        .expect("page");
    let mut doc = image.clone();

    let mut sig = Dict::new();
    sig.set(b"Type".to_vec(), Object::name("Sig"));
    sig.set(b"SubFilter".to_vec(), Object::name("adbe.pkcs7.detached"));
    sig.set(
        b"ByteRange".to_vec(),
        Object::Array(vec![0.into(), 10.into(), 20.into(), 30.into()]),
    );
    sig.set(
        b"Contents".to_vec(),
        Object::String(PdfString::literal(vec![1, 2, 3])),
    );
    let sig = doc.add(Object::Dict(sig));

    let mut widget = Dict::new();
    widget.set(b"Type".to_vec(), Object::name("Annot"));
    widget.set(b"Subtype".to_vec(), Object::name("Widget"));
    widget.set(b"FT".to_vec(), Object::name("Sig"));
    widget.set(b"T".to_vec(), Object::String(PdfString::text("Signature1")));
    widget.set(
        b"Rect".to_vec(),
        Object::Array(vec![8.into(), 8.into(), 80.into(), 40.into()]),
    );
    widget.set(b"P".to_vec(), Object::Ref(page));
    widget.set(b"V".to_vec(), Object::Ref(sig));
    let widget = doc.add(Object::Dict(widget));
    doc.update_dict(page, |dict| {
        dict.set(b"Annots".to_vec(), Object::Array(vec![Object::Ref(widget)]));
    })
    .expect("fixture");
    let mut acro = Dict::new();
    acro.set(b"Fields".to_vec(), Object::Array(vec![Object::Ref(widget)]));
    let acro = doc.add(Object::Dict(acro));
    let root = doc.root().expect("catalog");
    doc.update_dict(root, |catalog| {
        catalog.set(b"AcroForm".to_vec(), Object::Ref(acro));
        catalog.set(b"Perms".to_vec(), Object::Dict(Dict::new()));
    })
    .expect("fixture");
    write_full(&doc, &SaveOptions::default()).expect("write fixture")
}

/// A form with a text field and two signatures: one merged field/widget, one field whose widget
/// is a `/Kids` entry. Used to prove that non-signature fields and their annotations survive.
pub fn signed_form_pdf() -> Vec<u8> {
    let mut doc = Document::new_empty();
    let (_, page) = blank_page(&mut doc, 612, 792, b"BT /F1 24 Tf 72 720 Td (Form) Tj ET");

    let text = doc.add(Object::Dict({
        let mut d = Dict::new();
        d.set(b"Type".to_vec(), Object::name("Annot"));
        d.set(b"Subtype".to_vec(), Object::name("Widget"));
        d.set(b"FT".to_vec(), Object::name("Tx"));
        d.set(b"T".to_vec(), Object::String(PdfString::text("Name")));
        d.set(b"V".to_vec(), Object::String(PdfString::text("Ada")));
        d.set(
            b"Rect".to_vec(),
            Object::Array(vec![72.into(), 500.into(), 300.into(), 530.into()]),
        );
        d.set(b"P".to_vec(), Object::Ref(page));
        d
    }));

    let sig_a_value = signature_dict(&mut doc);
    let sig_a = doc.add(Object::Dict({
        let mut d = Dict::new();
        d.set(b"Type".to_vec(), Object::name("Annot"));
        d.set(b"Subtype".to_vec(), Object::name("Widget"));
        d.set(b"FT".to_vec(), Object::name("Sig"));
        d.set(b"T".to_vec(), Object::String(PdfString::text("Signature1")));
        d.set(b"V".to_vec(), Object::Ref(sig_a_value));
        d.set(
            b"Rect".to_vec(),
            Object::Array(vec![72.into(), 400.into(), 300.into(), 460.into()]),
        );
        d.set(b"P".to_vec(), Object::Ref(page));
        d
    }));

    let sig_b_value = signature_dict(&mut doc);
    let field_b = doc.add(Object::Dict({
        let mut d = Dict::new();
        d.set(b"FT".to_vec(), Object::name("Sig"));
        d.set(b"T".to_vec(), Object::String(PdfString::text("Signature2")));
        d.set(b"V".to_vec(), Object::Ref(sig_b_value));
        d
    }));
    let widget_b = doc.add(Object::Dict({
        let mut d = Dict::new();
        d.set(b"Type".to_vec(), Object::name("Annot"));
        d.set(b"Subtype".to_vec(), Object::name("Widget"));
        d.set(b"Parent".to_vec(), Object::Ref(field_b));
        d.set(
            b"Rect".to_vec(),
            Object::Array(vec![72.into(), 300.into(), 300.into(), 360.into()]),
        );
        d.set(b"P".to_vec(), Object::Ref(page));
        d
    }));
    doc.update_dict(field_b, |d| {
        d.set(b"Kids".to_vec(), Object::Array(vec![Object::Ref(widget_b)]));
    })
    .expect("fixture");

    doc.update_dict(page, |d| {
        d.set(
            b"Annots".to_vec(),
            Object::Array(vec![
                Object::Ref(text),
                Object::Ref(sig_a),
                Object::Ref(widget_b),
            ]),
        );
    })
    .expect("fixture");

    let acro = doc.add(Object::Dict({
        let mut d = Dict::new();
        d.set(
            b"Fields".to_vec(),
            Object::Array(vec![
                Object::Ref(text),
                Object::Ref(sig_a),
                Object::Ref(field_b),
            ]),
        );
        d.set(
            b"CO".to_vec(),
            Object::Array(vec![
                Object::Ref(sig_a),
                Object::Ref(field_b),
                Object::Ref(text),
            ]),
        );
        d.set(b"SigFlags".to_vec(), Object::Int(3));
        d
    }));
    let root = doc.root().expect("catalog");
    doc.update_dict(root, |catalog| {
        catalog.set(b"AcroForm".to_vec(), Object::Ref(acro));
        catalog.set(b"Perms".to_vec(), Object::Dict(Dict::new()));
    })
    .expect("fixture");
    write_full(&doc, &SaveOptions::default()).expect("write fixture")
}

/// The signed form fixture, encrypted with AES-256: user password "user", owner "owner".
pub fn encrypted_signed_pdf() -> Vec<u8> {
    let mut doc = Document::open(Arc::new(signed_pdf())).expect("fixture");
    doc.set_encryption(&NewEncryption {
        algorithm: Algorithm::Aes256,
        user_password: "user",
        owner_password: "owner",
        permissions: -4,
        encrypt_metadata: true,
        seed: [7; 32],
    })
    .expect("encrypt fixture");
    write_full(&doc, &SaveOptions::default()).expect("write fixture")
}

fn signature_dict(doc: &mut Document) -> ObjRef {
    doc.add(Object::Dict({
        let mut d = Dict::new();
        d.set(b"Type".to_vec(), Object::name("Sig"));
        d.set(b"SubFilter".to_vec(), Object::name("adbe.pkcs7.detached"));
        d.set(
            b"ByteRange".to_vec(),
            Object::Array(vec![0.into(), 100.into(), 200.into(), 300.into()]),
        );
        d.set(
            b"Contents".to_vec(),
            Object::String(PdfString::literal(vec![0x30, 0x82])),
        );
        d
    }))
}

fn blank_page(doc: &mut Document, width: i64, height: i64, content: &[u8]) -> (ObjRef, ObjRef) {
    let mut font = Dict::new();
    font.set(b"Type".to_vec(), Object::name("Font"));
    font.set(b"Subtype".to_vec(), Object::name("Type1"));
    font.set(b"BaseFont".to_vec(), Object::name("Helvetica"));
    let font = doc.add(Object::Dict(font));

    let mut fonts = Dict::new();
    fonts.set(b"F1".to_vec(), Object::Ref(font));
    let mut resources = Dict::new();
    resources.set(b"Font".to_vec(), Object::Dict(fonts));
    let resources = doc.add(Object::Dict(resources));
    let contents = doc.add(Object::Stream(Stream::flate(Dict::new(), content)));

    let pages = pages_ref(doc);
    let mut page = Dict::new();
    page.set(b"Type".to_vec(), Object::name("Page"));
    page.set(b"Parent".to_vec(), Object::Ref(pages));
    page.set(
        b"MediaBox".to_vec(),
        Object::Array(vec![0.into(), 0.into(), width.into(), height.into()]),
    );
    page.set(b"Resources".to_vec(), Object::Ref(resources));
    page.set(b"Contents".to_vec(), Object::Ref(contents));
    let page = doc.add(Object::Dict(page));
    doc.update_dict(pages, |dict| {
        dict.set(b"Kids".to_vec(), Object::Array(vec![Object::Ref(page)]));
        dict.set(b"Count".to_vec(), Object::Int(1));
    })
    .expect("fixture");
    (pages, page)
}

fn pages_ref(doc: &Document) -> ObjRef {
    let root = doc.root().expect("catalog");
    doc.get(root)
        .as_dict()
        .and_then(|d| d.reference(b"Pages"))
        .expect("page tree")
}

fn finish(doc: Document, _page: ObjRef) -> Vec<u8> {
    write_full(&doc, &SaveOptions::default()).expect("write fixture")
}

fn noise(len: usize) -> Vec<u8> {
    let mut state = 0x1234_5678u32;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        out.push((state >> 24) as u8);
    }
    out
}
