//! Digital-signature inspection and removal.
//!
//! PdfCraft deliberately preserves signatures: its optimizer refuses a signed document, and
//! clearing signature fields is still marked "planned" in its Acrobat-parity list. pdfskim adds
//! the destructive half on top of PdfCraft's COS layer: it removes the signature fields (and
//! their widgets), `/Perms` and `/DSS`, then writes a **full** rewrite so that the old revisions
//! that hold the signed bytes are not carried into the output.
//!
//! What this cannot remove: pixels a signature was flattened into (they are page content), and
//! signatures inside an XFA payload (pdfskim reports `xfa` and leaves the XML alone).

use std::collections::HashSet;

use anyhow::{Result, bail};
use pdfcraft_cos::{Dict, Document, ObjRef, Object};

/// What a document contains, before or after a strip.
#[derive(Clone, Debug, Default)]
pub struct SignatureReport {
    /// Signature fields, signed or empty.
    pub fields: usize,
    /// Signature fields that hold a signature (`/V`).
    pub signed_fields: usize,
    /// Signature dictionaries reachable in the file (`/ByteRange` + `/Contents`, or `/Type /Sig`).
    pub dictionaries: usize,
    /// `/Perms` on the catalog (certification or usage-rights signature).
    pub perms: bool,
    /// `/DSS` on the catalog (validation data).
    pub dss: bool,
    /// An XFA payload is present; signatures inside it are not inspected or removed.
    pub xfa: bool,
    /// Partial names (`/T`) of the signature fields.
    pub names: Vec<String>,
}

impl SignatureReport {
    pub fn signed(&self) -> bool {
        self.signed_fields > 0 || self.perms
    }

    pub fn any(&self) -> bool {
        self.signed() || self.dss || self.dictionaries > 0
    }
}

/// Inspect a document. `count_dicts` walks every object (needed before a strip and by `info`);
/// the compress path leaves it off because field and `/Perms` checks are enough there.
pub fn inspect(doc: &Document, count_dicts: bool) -> SignatureReport {
    let mut report = SignatureReport {
        dictionaries: if count_dicts {
            count_signature_dicts(doc)
        } else {
            0
        },
        ..Default::default()
    };
    let Some(root) = doc.root() else {
        return report;
    };
    let catalog = doc.get(root);
    let Some(catalog) = catalog.as_dict() else {
        return report;
    };
    report.perms = catalog.contains(b"Perms");
    report.dss = catalog.contains(b"DSS");
    let Some(acro) = catalog.get(b"AcroForm").and_then(|o| as_dict(doc, o)) else {
        return report;
    };
    report.xfa = acro.contains(b"XFA");
    let Some(fields) = acro.get(b"Fields").and_then(|o| as_array(doc, o)) else {
        return report;
    };
    walk_fields(doc, &fields, None, 0, &mut |dict, is_sig, name| {
        if is_sig {
            report.fields += 1;
            if let Some(name) = name {
                report.names.push(name);
            }
            if has_value(doc, dict) {
                report.signed_fields += 1;
            }
        }
    });
    report
}

/// The outcome of a strip.
#[derive(Clone, Debug)]
pub struct StripOutcome {
    /// What the document held before the strip.
    pub report: SignatureReport,
    /// The document changed, so it must be rewritten for the removal to take effect.
    pub changed: bool,
    /// Widget annotations taken off the pages.
    pub annotations_removed: usize,
}

/// Remove every signature field, the widgets that draw them, `/Perms` and `/DSS`.
///
/// Returns what was found. When nothing was found, the document is left untouched (the caller
/// can skip the rewrite); when something was, the document is put in full-save-with-new-id mode.
pub fn strip(doc: &mut Document) -> Result<StripOutcome> {
    let root = doc
        .root()
        .ok_or_else(|| anyhow::anyhow!("the document has no catalog"))?;
    let before = inspect(doc, true);
    let mut state = StripState::default();

    if let Some(acro) = ensure_acroform(doc, root)? {
        let xfa = acro_dict(doc, acro).is_some_and(|d| d.contains(b"XFA"));
        if let Some(fields) = ensure_array(doc, acro, b"Fields")? {
            for field in normalize_array(doc, fields)? {
                normalize_field(doc, field, 0)?;
            }
            let fields_now = normalize_array(doc, fields)?;
            let mut kept = Vec::with_capacity(fields_now.len());
            for field in fields_now {
                if prune_field(doc, field, None, &mut state)? {
                    kept.push(Object::Ref(field));
                }
            }
            doc.set(fields, Object::Array(kept.clone()));
            if kept.is_empty() && !xfa {
                doc.update_dict(root, |catalog| {
                    catalog.remove(b"AcroForm");
                })?;
            } else {
                clean_acroform(doc, acro, &state)?;
            }
        } else {
            clean_acroform(doc, acro, &state)?;
        }
    }

    state.annotations_removed = remove_widget_annotations(doc, &state.widgets)?;

    doc.update_dict(root, |catalog| {
        state.perms_removed = catalog.remove(b"Perms").is_some();
        state.dss_removed = catalog.remove(b"DSS").is_some();
    })?;

    let changed = !state.removed_fields.is_empty()
        || state.perms_removed
        || state.dss_removed
        || state.annotations_removed > 0
        || before.dictionaries > 0;
    if changed {
        doc.require_full_save_with_new_id();
    }

    Ok(StripOutcome {
        report: SignatureReport {
            fields: before.fields,
            signed_fields: before.signed_fields,
            dictionaries: before.dictionaries,
            perms: before.perms,
            dss: before.dss,
            xfa: before.xfa,
            names: state.removed_names,
        },
        changed,
        annotations_removed: state.annotations_removed,
    })
}

/// Number of signature dictionaries in the document.
pub fn count_signature_dicts(doc: &Document) -> usize {
    doc.scan_objects()
        .filter(|(_, object)| match &**object {
            Object::Dict(dict) => is_signature_dict(dict),
            _ => false,
        })
        .count()
}

fn is_signature_dict(dict: &Dict) -> bool {
    dict.contains(b"ByteRange") || dict.name(b"Type") == Some(b"Sig")
}

#[derive(Default)]
struct StripState {
    removed_fields: Vec<ObjRef>,
    removed_names: Vec<String>,
    removed_refs: HashSet<ObjRef>,
    widgets: HashSet<ObjRef>,
    annotations_removed: usize,
    perms_removed: bool,
    dss_removed: bool,
}

fn walk_fields(
    doc: &Document,
    fields: &[Object],
    inherited_ft: Option<&[u8]>,
    depth: usize,
    visit: &mut dyn FnMut(&Dict, bool, Option<String>),
) {
    if depth > 64 {
        return;
    }
    for field in fields {
        let Some(dict) = as_dict(doc, field) else {
            continue;
        };
        let ft = dict.name(b"FT").or(inherited_ft);
        let is_sig = ft == Some(b"Sig");
        if is_sig {
            visit(&dict, true, field_name(&dict));
        }
        if let Some(kids) = dict.get(b"Kids").and_then(|o| as_array(doc, o)) {
            // A signature field is terminal; its kids are widgets, not fields.
            if !is_sig {
                walk_fields(doc, &kids, ft, depth + 1, visit);
            }
        }
    }
}

fn has_value(doc: &Document, dict: &Dict) -> bool {
    dict.get(b"V")
        .is_some_and(|value| !matches!(&*doc.resolve(value), Object::Null))
}

fn field_name(dict: &Dict) -> Option<String> {
    dict.get(b"T")
        .and_then(|o| o.as_string())
        .map(|s| s.to_text())
        .filter(|s| !s.is_empty())
}

fn as_dict(doc: &Document, object: &Object) -> Option<Dict> {
    match &*doc.resolve(object) {
        Object::Dict(dict) => Some(dict.clone()),
        Object::Stream(stream) => Some(stream.dict.clone()),
        _ => None,
    }
}

fn as_array(doc: &Document, object: &Object) -> Option<Vec<Object>> {
    match &*doc.resolve(object) {
        Object::Array(array) => Some(array.clone()),
        _ => None,
    }
}

fn acro_dict(doc: &Document, acro: ObjRef) -> Option<Dict> {
    doc.get(acro).as_dict().cloned()
}

/// Make `/AcroForm` indirect, so every write below can go through a reference.
fn ensure_acroform(doc: &mut Document, root: ObjRef) -> Result<Option<ObjRef>> {
    let Some(value) = doc
        .get(root)
        .as_dict()
        .and_then(|c| c.get(b"AcroForm").cloned())
    else {
        return Ok(None);
    };
    if let Some(reference) = value.as_ref() {
        if doc.get(reference).as_dict().is_some() {
            return Ok(Some(reference));
        }
        return Ok(None);
    }
    if let Some(dict) = value.as_dict() {
        let reference = doc.add(Object::Dict(dict.clone()));
        doc.update_dict(root, |catalog| {
            catalog.set(b"AcroForm".to_vec(), Object::Ref(reference))
        })?;
        return Ok(Some(reference));
    }
    Ok(None)
}

/// Make `owner / key` an indirect array, so its entries can be rewritten in place.
fn ensure_array(doc: &mut Document, owner: ObjRef, key: &[u8]) -> Result<Option<ObjRef>> {
    let Some(value) = doc.get(owner).as_dict().and_then(|d| d.get(key).cloned()) else {
        return Ok(None);
    };
    if let Some(reference) = value.as_ref() {
        return Ok(doc.get(reference).as_array().is_some().then_some(reference));
    }
    if let Some(array) = value.as_array() {
        let reference = doc.add(Object::Array(array.clone()));
        doc.update_dict(owner, |d| d.set(key.to_vec(), Object::Ref(reference)))?;
        return Ok(Some(reference));
    }
    Ok(None)
}

/// Entry references of an array, turning direct field dictionaries into indirect objects.
fn normalize_array(doc: &mut Document, array: ObjRef) -> Result<Vec<ObjRef>> {
    let entries = doc.get(array).as_array().cloned().unwrap_or_default();
    let mut refs = Vec::with_capacity(entries.len());
    let mut changed = false;
    for entry in entries {
        if let Some(reference) = entry.as_ref() {
            refs.push(reference);
        } else if matches!(&*doc.resolve(&entry), Object::Dict(_)) {
            refs.push(doc.add(entry));
            changed = true;
        }
    }
    if changed {
        doc.set(
            array,
            Object::Array(refs.iter().map(|r| Object::Ref(*r)).collect()),
        );
    }
    Ok(refs)
}

fn normalize_field(doc: &mut Document, field: ObjRef, depth: usize) -> Result<()> {
    if depth > 64 {
        bail!("the form field tree is deeper than 64 levels");
    }
    let dit = doc.get(field).as_dict().cloned().unwrap_or_default();
    if !dit.contains(b"Kids") {
        return Ok(());
    }
    if let Some(kids) = ensure_array(doc, field, b"Kids")? {
        for kid in normalize_array(doc, kids)? {
            normalize_field(doc, kid, depth + 1)?;
        }
    }
    Ok(())
}

fn prune_field(
    doc: &mut Document,
    field: ObjRef,
    inherited_ft: Option<Vec<u8>>,
    state: &mut StripState,
) -> Result<bool> {
    let dict = doc.get(field).as_dict().cloned().unwrap_or_default();
    let ft = dict.name(b"FT").map(|n| n.to_vec()).or(inherited_ft);
    if ft.as_deref() == Some(b"Sig") {
        state.removed_fields.push(field);
        state.removed_refs.insert(field);
        if let Some(name) = field_name(&dict) {
            state.removed_names.push(name);
        }
        collect_subtree(doc, field, &mut state.widgets, 0);
        return Ok(false);
    }
    if let Some(kids) = dict.reference(b"Kids") {
        let entries = doc.get(kids).as_array().cloned().unwrap_or_default();
        let total = entries.len();
        let mut kept = Vec::with_capacity(total);
        for entry in entries {
            match entry.as_ref() {
                Some(kid) => {
                    if prune_field(doc, kid, ft.clone(), state)? {
                        kept.push(entry);
                    }
                }
                None => kept.push(entry),
            }
        }
        if kept.len() != total {
            doc.set(kids, Object::Array(kept.clone()));
        }
        // A non-terminal field whose every child was a signature is an empty group now.
        if kept.is_empty() && total > 0 && !dict.contains(b"V") {
            state.removed_refs.insert(field);
            return Ok(false);
        }
    }
    Ok(true)
}

fn collect_subtree(doc: &Document, field: ObjRef, widgets: &mut HashSet<ObjRef>, depth: usize) {
    if depth > 64 || !widgets.insert(field) {
        return;
    }
    let Some(dict) = doc.get(field).as_dict().cloned() else {
        return;
    };
    let Some(kids) = dict.reference(b"Kids") else {
        return;
    };
    let Some(entries) = doc.get(kids).as_array().cloned() else {
        return;
    };
    for entry in entries {
        if let Some(kid) = entry.as_ref() {
            collect_subtree(doc, kid, widgets, depth + 1);
        }
    }
}

fn clean_acroform(doc: &mut Document, acro: ObjRef, state: &StripState) -> Result<()> {
    doc.update_dict(acro, |dict| {
        dict.remove(b"SigFlags");
    })?;
    if let Some(co) = ensure_array(doc, acro, b"CO")? {
        let entries = doc.get(co).as_array().cloned().unwrap_or_default();
        let kept: Vec<Object> = entries
            .into_iter()
            .filter(|entry| {
                !entry
                    .as_ref()
                    .is_some_and(|r| state.removed_refs.contains(&r))
            })
            .collect();
        if kept.is_empty() {
            doc.update_dict(acro, |dict| {
                dict.remove(b"CO");
            })?;
            doc.set(co, Object::Array(Vec::new()));
        } else {
            doc.set(co, Object::Array(kept));
        }
    }
    Ok(())
}

fn remove_widget_annotations(doc: &mut Document, widgets: &HashSet<ObjRef>) -> Result<usize> {
    let pages = pdfcraft_annot::page_refs(doc)
        .map_err(|e| anyhow::anyhow!("cannot walk the page tree: {e}"))?;
    let mut removed = 0;
    for page in pages {
        let Some(annots) = doc
            .get(page)
            .as_dict()
            .and_then(|d| d.get(b"Annots").cloned())
        else {
            continue;
        };
        let Some(entries) = as_array(doc, &annots) else {
            continue;
        };
        let total = entries.len();
        // Remove the signature field's widgets, and any signature widget the field tree did not
        // list (some producers keep the widget only on the page).
        let kept: Vec<Object> = entries
            .into_iter()
            .filter(|entry| {
                if entry.as_ref().is_some_and(|r| widgets.contains(&r)) {
                    return false;
                }
                !as_dict(doc, entry).is_some_and(|dict| dict.name(b"FT") == Some(b"Sig"))
            })
            .collect();
        removed += total - kept.len();
        if kept.len() == total {
            continue;
        }
        if let Some(reference) = annots.as_ref() {
            doc.set(reference, Object::Array(kept));
        } else {
            doc.update_dict(page, |dict| {
                dict.set(b"Annots".to_vec(), Object::Array(kept))
            })?;
        }
    }
    Ok(removed)
}
