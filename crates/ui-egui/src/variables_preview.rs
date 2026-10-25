//! Live canvas preview for the Variables dialog's Data Sets page and Apply Data Set (their
//! Preview check box, as in Photoshop): the canvas shows the committed document with the shown
//! data set applied. Nothing is recorded: OK applies through `image.applyDataSet` (one history
//! step) and Cancel just drops the preview.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use photocraft_doc::{DocId, Document};
use serde_json::{Value, json};

use crate::PhotocraftApp;

/// Preview keys (`canvas::display_doc`) are `TAG` plus a hash of what is shown.
const TAG: u64 = 1 << 48;

/// The preview last built, and its key; or why it could not be built (shown in the dialog).
pub(crate) struct Preview {
    dialog: u64,
    key: u64,
    shown: Result<Arc<Document>, String>,
}

/// The data set a dialog previews on `doc`: its definitions and the shown set, when the dialog
/// is on a page with Preview and the check box is on.
fn previewed(fields: &serde_json::Map<String, Value>, doc: DocId) -> Option<(Value, Value)> {
    if !crate::variables_ui::previewing(fields) || fields.get("__document").and_then(Value::as_u64) != Some(doc.0) {
        return None;
    }
    let state = fields.get("__variables")?;
    let sets = state.get("dataSets")?.as_array()?;
    let cur = fields.get("__cur").and_then(Value::as_u64).and_then(|c| usize::try_from(c).ok()).unwrap_or(0);
    let set = sets.get(cur).or_else(|| sets.first())?.clone();
    Some((state.get("defs").cloned().unwrap_or_else(|| json!([])), set))
}

pub(crate) fn display_doc(app: &mut PhotocraftApp, idx: usize) -> Option<(Arc<Document>, u64)> {
    let st = app.session.documents().get(idx)?;
    let (doc_id, revision) = (st.doc.id, st.revision);
    let found = app.ui.dialogs.iter().rev().find(|d| crate::variables_ui::owns(&d.fields)).map(|d| (d.id, previewed(&d.fields, doc_id)));
    let Some((dialog, Some((defs, set)))) = found else {
        if found.is_none() {
            app.variables_preview = None;
        }
        return None;
    };
    // The dialog identity is part of the key: cancelling and reopening at the same revision must
    // not reuse an old preview's pixels in the canvas caches.
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    (doc_id.0, revision, dialog, defs.to_string(), set.to_string()).hash(&mut hash);
    let key = TAG | (hash.finish() & ((1 << 36) - 1));
    if let Some(p) = app.variables_preview.as_ref().filter(|p| p.key == key) {
        return p.shown.as_ref().ok().map(|shown| (shown.clone(), key));
    }
    let st = app.session.documents().get(idx)?;
    let shown = photocraft_engine::preview_data_set(&app.session, &st.doc, &defs, &set)
        .map(|doc| Arc::new(photocraft_engine::mode_cmds::display_document(&doc).unwrap_or(doc)))
        .map_err(|e| e.to_string());
    let out = shown.as_ref().ok().map(|shown| (shown.clone(), key));
    app.variables_preview = Some(Preview { dialog, key, shown });
    out
}

/// Why dialog `dialog`'s preview could not be shown, if it could not.
pub(crate) fn error(app: &PhotocraftApp, dialog: u64) -> Option<&str> {
    app.variables_preview.as_ref().filter(|p| p.dialog == dialog).and_then(|p| p.shown.as_ref().err()).map(String::as_str)
}
