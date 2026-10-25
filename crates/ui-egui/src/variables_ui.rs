//! Front end for Image › Variables (Define… / Data Sets…) and Apply Data Set — Photoshop's
//! data-driven graphics dialog. All editing drives the headless engine commands
//! (`image.variables.define`, `image.variables.dataSets`, `image.applyDataSet`), so the control
//! channel can drive this with `ui.dialog.set` / `confirm` exactly like a human.
//!
//! The dialog keeps its working state as JSON in the `__variables` field, shaped like the engine
//! command params, so `confirm` passes it straight through.

use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::file_dialog::{FileDialogAnswer, FileDialogRequest};
use crate::state::DialogKind;
use crate::theme::Tokens;

/// This dialog owns a field set when it carries `__variables`.
pub fn owns(fields: &Map<String, Value>) -> bool {
    fields.contains_key("__variables")
}

/// Alt+Cancel (Reset): restore `__variables` from the open-time snapshot.
pub fn reset(fields: &mut Map<String, Value>) {
    if !owns(fields) {
        return;
    }
    if let Some(orig) = fields.get("__variablesOrig").cloned() {
        fields.insert("__variables".into(), orig);
    }
}

/// What a layer can be bound to: every layer has Visibility, type layers add Text Replacement and
/// pixel layers Pixel Replacement, as Photoshop's Define page offers them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LayerKind {
    Text,
    Pixel,
    Other,
}

impl LayerKind {
    fn allows(self, ty: &str) -> bool {
        match ty {
            "textReplacement" => self == LayerKind::Text,
            "pixelReplacement" => self == LayerKind::Pixel,
            _ => true,
        }
    }
}

/// Layers available to bind, top to bottom: (id, display name, kind).
fn layer_options(app: &PhotocraftApp) -> Vec<(u64, String, LayerKind)> {
    let Some(st) = app.session.active() else { return Vec::new() };
    st.doc
        .walk()
        .iter()
        .map(|(_, _, l)| {
            let kind = match l.content {
                photocraft_doc::LayerContent::Text(_) => LayerKind::Text,
                photocraft_doc::LayerContent::Raster(_) => LayerKind::Pixel,
                _ => LayerKind::Other,
            };
            (l.id.0, l.name.clone(), kind)
        })
        .collect()
}

/// Menu entry points. Returns None for ids this module doesn't own.
pub fn menu(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let has_params = params.as_object().is_some_and(|o| !o.is_empty());
    match id {
        "image.variables.define" | "image.variables.dataSets" if !has_params => {
            let page = if id.ends_with("dataSets") { "dataSets" } else { "define" };
            Some(Ok(open(app, page, "Variables", false)))
        }
        // No data set named: Photoshop's Apply Data Set dialog, a list to pick one from, with
        // Preview on.
        "image.applyDataSet" if !has_params => Some(Ok(open(app, "apply", "Apply Data Set", true))),
        _ => None,
    }
}

/// Open the dialog on `page` for the active document's variables.
fn open(app: &mut PhotocraftApp, page: &str, label: &str, preview: bool) -> Value {
    let mut f = Map::new();
    let vars = app.session.active().map(|st| (to_state(&st.doc.variables), st.doc.variables.active, st.doc.id.0));
    let (state, active, document) = vars.unwrap_or_else(|| (json!({"defs": [], "dataSets": []}), None, 0));
    // Snapshot at open so Alt+Reset restores the document's variables.
    f.insert("__variablesOrig".into(), state.clone());
    f.insert("__variables".into(), state);
    f.insert("__page".into(), json!(page));
    f.insert("__cur".into(), json!(active.unwrap_or(0)));
    // The document the dialog edits: Preview and Import read it, whichever is active later.
    f.insert("__document".into(), json!(document));
    f.insert("__preview".into(), json!(preview));
    f.insert("__importFirstColumn".into(), json!(true));
    f.insert("__importReplace".into(), json!(true));
    f.insert("__label".into(), json!(label));
    json!({"dialog": app.ui.open_dialog(DialogKind::Command, f)})
}

/// Whether the canvas previews the shown data set: Preview on, on the Data Sets page or in
/// Apply Data Set.
pub fn previewing(fields: &Map<String, Value>) -> bool {
    matches!(fields.get("__page").and_then(Value::as_str), Some("dataSets" | "apply")) && fields.get("__preview").and_then(Value::as_bool).unwrap_or(false)
}

/// The Preview check box, and why the preview can't be shown when it can't.
fn preview_row(app: &PhotocraftApp, ui: &mut egui::Ui, fields: &mut Map<String, Value>, id: u64) {
    let mut on = fields.get("__preview").and_then(Value::as_bool).unwrap_or(false);
    if ui.checkbox(&mut on, tl!("Preview")).changed() {
        fields.insert("__preview".into(), json!(on));
    }
    if on && let Some(e) = crate::variables_preview::error(app, id) {
        ui.colored_label(ui.visuals().error_fg_color, e);
    }
}

/// What the Data Sets page asked for this frame: a file dialog the body opens once the frame's
/// edits are saved.
#[derive(Default)]
struct Requests {
    /// Choose an image for this Pixel Replacement variable of the shown data set.
    pick_image: Option<String>,
    /// Import data sets from a text file.
    import: bool,
}

/// Images a Pixel Replacement value can name.
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "tif", "tiff", "psd", "psb", "bmp", "gif", "webp", "tga"];

/// Choose… for a Pixel Replacement value: the picked path lands in dialog `id`'s data set `cur`.
fn pick_image(app: &mut PhotocraftApp, id: u64, cur: usize, variable: String) -> Result<Value, String> {
    let initial_dir = crate::file_dialog::last_used_dir(&app.ui.recent_files);
    let extensions = Some(IMAGE_EXTS.iter().map(|e| e.to_string()).collect());
    app.ask_file(FileDialogRequest::Open { multiple: false, initial_dir, extensions }, move |app, answer| {
        let FileDialogAnswer::Paths(paths) = answer else { return Err(tl!("Pixel Replacement reads image files from disk").to_string()) };
        let path = paths.into_iter().next().ok_or(crate::file_dialog::CANCELLED)?;
        let d = app.ui.dialog_mut(id).ok_or("the dialog was closed")?;
        let mut state = normalize(d.fields.get("__variables").unwrap_or(&Value::Null));
        set_value(&mut state, cur, &variable, json!({"variable": variable, "kind": "pixels", "value": path}));
        d.fields.insert("__variables".into(), state);
        Ok(Value::Null)
    })
}

/// Store `value` as `variable`'s value in data set `cur` of the dialog state.
fn set_value(state: &mut Value, cur: usize, variable: &str, value: Value) {
    let Some(values) = state.get_mut("dataSets").and_then(Value::as_array_mut).and_then(|s| s.get_mut(cur)).and_then(|s| s.get_mut("values")).and_then(Value::as_array_mut) else {
        return;
    };
    match values.iter_mut().find(|v| v.get("variable").and_then(Value::as_str) == Some(variable)) {
        Some(v) => *v = value,
        None => values.push(value),
    }
}

/// Import…: read a comma- or tab-delimited file into dialog `id`, for the definitions it holds.
fn import(app: &mut PhotocraftApp, id: u64) -> Result<Value, String> {
    app.pick_file_bytes_filtered(&["csv", "txt", "tsv"], move |app, _name, bytes| {
        let fields = app.ui.dialog_mut(id).ok_or("the dialog was closed")?.fields.clone();
        let mut state = normalize(fields.get("__variables").unwrap_or(&Value::Null));
        let document = fields.get("__document").and_then(Value::as_u64);
        let doc = app.session.documents().iter().find(|d| Some(d.doc.id.0) == document).map(|d| d.doc.clone()).ok_or("the document was closed")?;
        let first = fields.get("__importFirstColumn").and_then(Value::as_bool).unwrap_or(true);
        let replace = fields.get("__importReplace").and_then(Value::as_bool).unwrap_or(true);
        let text = String::from_utf8_lossy(&bytes);
        let defs = state.get("defs").cloned().unwrap_or_else(|| json!([]));
        let imported = photocraft_engine::data_sets_from_text(&doc, &defs, &text, &json!({"firstColumnNames": first})).map_err(|e| e.to_string())?;
        let (state, cur) = merged_import(state.take(), imported, replace);
        let d = app.ui.dialog_mut(id).ok_or("the dialog was closed")?;
        d.fields.insert("__variables".into(), state);
        d.fields.insert("__cur".into(), json!(cur));
        Ok(Value::Null)
    })
}

/// The dialog state with `imported` data sets in place of its own (`replace`) or after them,
/// renamed apart from the ones there; and the first imported set's index.
fn merged_import(mut state: Value, imported: Value, replace: bool) -> (Value, usize) {
    let imported = imported.as_array().cloned().unwrap_or_default();
    let mut sets = if replace { Vec::new() } else { state.get("dataSets").and_then(Value::as_array).cloned().unwrap_or_default() };
    let first = sets.len();
    for mut set in imported {
        let name = set.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
        let taken = |n: &str, sets: &[Value]| sets.iter().any(|s| s.get("name").and_then(Value::as_str) == Some(n));
        if taken(&name, &sets) {
            let unique = (2..).map(|i| format!("{name} {i}")).find(|n| !taken(n, &sets)).unwrap_or(name);
            set["name"] = json!(unique);
        }
        sets.push(set);
    }
    state["dataSets"] = Value::Array(sets);
    (state, first)
}

/// Serialise the document's variables into the dialog's editable JSON shape.
pub fn to_state(v: &photocraft_doc::Variables) -> Value {
    json!({
        "defs": v.defs.iter().map(|d| {
            let (ty, method, align, clip) = match &d.kind {
                photocraft_doc::VarKind::Visibility => ("visibility", "fit", "center", false),
                photocraft_doc::VarKind::TextReplacement => ("textReplacement", "fit", "center", false),
                photocraft_doc::VarKind::PixelReplacement { method, align, clip } => {
                    (_pixel(), _m(method), _a(align), *clip)
                }
            };
            json!({"name": d.name, "layer": d.layer.0, "type": ty, "method": method, "align": align, "clip": clip})
        }).collect::<Vec<_>>(),
        "dataSets": v.data_sets.iter().map(|s| json!({
            "name": s.name,
            "values": s.values.iter().map(|dv| {
                let (kind, value) = match &dv.value {
                    photocraft_doc::variables::Value::Visibility(b) => ("visibility", json!(b)),
                    photocraft_doc::variables::Value::Text(t) => ("text", json!(t)),
                    photocraft_doc::variables::Value::Pixels(p) => ("pixels", json!(p)),
                };
                json!({"variable": dv.variable, "kind": kind, "value": value})
            }).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

fn _pixel() -> &'static str {
    "pixelReplacement"
}
fn _m(m: &photocraft_doc::PixelMethod) -> &'static str {
    use photocraft_doc::PixelMethod::*;
    match m {
        Fit => "fit",
        Fill => "fill",
        AsIs => "asIs",
        Conform => "conform",
    }
}
fn _a(a: &photocraft_doc::PixelAlign) -> &'static str {
    use photocraft_doc::PixelAlign::*;
    match a {
        TopLeft => "topLeft",
        TopCenter => "topCenter",
        TopRight => "topRight",
        CenterLeft => "centerLeft",
        Center => "center",
        CenterRight => "centerRight",
        BottomLeft => "bottomLeft",
        BottomCenter => "bottomCenter",
        BottomRight => "bottomRight",
    }
}

const TYPES: &[(&str, &str)] = &[("visibility", "Visibility"), ("textReplacement", "Text Replacement"), ("pixelReplacement", "Pixel Replacement")];
const METHODS: &[(&str, &str)] = &[("fit", "Fit"), ("fill", "Fill"), ("conform", "Conform"), ("asIs", "As Is")];
const ALIGNS: &[(&str, &str)] = &[
    ("topLeft", "Top Left"),
    ("topCenter", "Top Center"),
    ("topRight", "Top Right"),
    ("centerLeft", "Center Left"),
    ("center", "Center"),
    ("centerRight", "Center Right"),
    ("bottomLeft", "Bottom Left"),
    ("bottomCenter", "Bottom Center"),
    ("bottomRight", "Bottom Right"),
];

/// Render the dialog. Mutates `fields["__variables"]` and `fields["__page"]`.
pub fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, fields: &mut Map<String, Value>, id: u64) {
    let t = Tokens::get(ui.ctx());
    let layers = layer_options(app);
    let mut state = normalize(fields.get("__variables").unwrap_or(&Value::Null));
    let mut page = fields.get("__page").and_then(Value::as_str).unwrap_or("define").to_string();

    ui.set_min_width(420.0);
    if page == "apply" {
        apply_page(ui, &mut state, fields);
        preview_row(app, ui, fields, id);
        fields.insert("__variables".into(), state);
        return;
    }
    // Page switcher.
    ui.horizontal(|ui| {
        for (id, label) in [("define", tl!("Define")), ("dataSets", tl!("Data Sets"))] {
            let sel = page == id;
            if ui.selectable_label(sel, tl!(&label)).clicked() {
                page = id.to_string();
            }
        }
    });
    ui.separator();

    if layers.is_empty() {
        ui.weak(tl!("Open a document to define variables."));
    } else if page == "define" {
        define_page(ui, &t, &layers, &mut state);
    } else {
        let mut requests = Requests::default();
        data_sets_page(ui, &t, &mut state, fields, &mut requests);
        preview_row(app, ui, fields, id);
        if requests.pick_image.is_some() || requests.import {
            // The file dialog's answer lands in the dialog's own fields: save this frame's edits first.
            fields.insert("__variables".into(), state.clone());
            fields.insert("__page".into(), json!(page));
            if let Some(d) = app.ui.dialog_mut(id) {
                d.fields = fields.clone();
            }
            let cur = data_set_index(fields, state.get("dataSets").and_then(Value::as_array).map_or(0, Vec::len));
            let r = match requests.pick_image {
                Some(variable) => pick_image(app, id, cur, variable),
                None => import(app, id),
            };
            if let Err(e) = r
                && e != crate::file_dialog::CANCELLED
            {
                app.ui.status = e;
                app.ui.status_error = true;
            }
        }
    }

    fields.insert("__variables".into(), state);
    fields.insert("__page".into(), json!(page));
}

/// The dialog state with only object entries in `defs`, `dataSets` and each set's `values`.
/// `ui.dialog.set` can store any JSON in `__variables`, and the pages write fields into entries
/// (`d["name"] = …`), which panics on a number or a string.
fn normalize(state: &Value) -> Value {
    let objects =
        |v: Option<&Value>| -> Vec<Value> { v.and_then(Value::as_array).map(|a| a.iter().filter(|e| e.is_object()).cloned().collect()).unwrap_or_default() };
    let sets: Vec<Value> = objects(state.get("dataSets"))
        .into_iter()
        .map(|mut set| {
            set["values"] = Value::Array(objects(set.get("values")));
            set
        })
        .collect();
    json!({"defs": objects(state.get("defs")), "dataSets": sets})
}

fn define_page(ui: &mut egui::Ui, _t: &Tokens, layers: &[(u64, String, LayerKind)], state: &mut Value) {
    let Some(defs) = state.get_mut("defs").and_then(Value::as_array_mut) else { return };
    ui.label(tl!("Variables bind a layer's visibility, text or pixels to a named data slot."));
    ui.add_space(4.0);
    let mut remove = None;
    for (i, d) in defs.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            // name
            let mut name = d.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            if ui.add(egui::TextEdit::singleline(&mut name).hint_text(tl!("name")).desired_width(110.0)).changed() {
                d["name"] = json!(name);
            }
            // layer
            let mut layer = d.get("layer").and_then(Value::as_u64).unwrap_or(layers[0].0);
            let opts: Vec<(u64, &str)> = layers.iter().map(|(id, n, _)| (*id, n.as_str())).collect();
            if crate::widgets::dropdown(ui, &format!("vl{i}"), &mut layer, &opts, 120.0) {
                d["layer"] = json!(layer);
            }
            // type: only the kinds this layer supports
            let kind = layers.iter().find(|(id, _, _)| *id == layer).map_or(LayerKind::Other, |l| l.2);
            let mut ty = d.get("type").and_then(Value::as_str).unwrap_or("visibility").to_string();
            if !kind.allows(&ty) {
                ty = "visibility".into();
                d["type"] = json!(ty);
            }
            let types: Vec<(String, &str)> = TYPES.iter().filter(|(a, _)| kind.allows(a)).map(|(a, b)| (a.to_string(), *b)).collect();
            if crate::widgets::dropdown(ui, &format!("vt{i}"), &mut ty, &types, 150.0) {
                d["type"] = json!(ty);
            }
            if ty == "pixelReplacement" {
                let mut m = d.get("method").and_then(Value::as_str).unwrap_or("fit").to_string();
                if crate::widgets::dropdown(ui, &format!("vm{i}"), &mut m, &METHODS.iter().map(|(a, b)| (a.to_string(), *b)).collect::<Vec<_>>(), 80.0) {
                    d["method"] = json!(m);
                }
                let mut a = d.get("align").and_then(Value::as_str).unwrap_or("center").to_string();
                if crate::widgets::dropdown(ui, &format!("va{i}"), &mut a, &ALIGNS.iter().map(|(a, b)| (a.to_string(), *b)).collect::<Vec<_>>(), 110.0) {
                    d["align"] = json!(a);
                }
                let mut clip = d.get("clip").and_then(Value::as_bool).unwrap_or(false);
                if ui.checkbox(&mut clip, tl!("Clip")).on_hover_text(tl!("Clip Image to Bounding Box")).changed() {
                    d["clip"] = json!(clip);
                }
            }
            if ui.button("−").on_hover_text(tl!("Remove variable")).clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        defs.remove(i);
    }
    ui.add_space(4.0);
    if ui.button(tl!("+ Add variable")).clicked() {
        let n = defs.len() + 1;
        defs.push(json!({"name": format!("var{n}"), "layer": layers[0].0, "type": "visibility", "method": "fit", "align": "center", "clip": false}));
    }
}

fn data_set_index(fields: &Map<String, Value>, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    fields.get("__cur").and_then(Value::as_u64).and_then(|v| usize::try_from(v).ok()).filter(|&i| i < len).unwrap_or(0)
}

fn data_sets_page(ui: &mut egui::Ui, _t: &Tokens, state: &mut Value, fields: &mut Map<String, Value>, requests: &mut Requests) {
    let def_meta: Vec<(String, String)> = state
        .get("defs")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|d| {
                    (d.get("name").and_then(Value::as_str).unwrap_or("").to_string(), d.get("type").and_then(Value::as_str).unwrap_or("visibility").to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    if def_meta.is_empty() {
        ui.weak(tl!("Define at least one variable first."));
        return;
    }
    let Some(sets) = state.get_mut("dataSets").and_then(Value::as_array_mut) else { return };
    let mut cur = data_set_index(fields, sets.len());

    ui.horizontal(|ui| {
        if ui.button("◀").clicked() && cur > 0 {
            cur -= 1;
        }
        let name = sets
            .get(cur)
            .and_then(|s| s.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                let empty = "(none)";
                tl!(empty)
            })
            .to_string();
        ui.label({
            let template = "Data Set: {name}  ({index}/{total})";
            crate::i18n::fmt(
                tl!(template),
                &[("name", &name), ("index", &(if sets.is_empty() { 0 } else { cur + 1 }).to_string()), ("total", &sets.len().to_string())],
            )
        });
        if ui.button("▶").clicked() && cur + 1 < sets.len() {
            cur += 1;
        }
        if ui.button(tl!("New")).on_hover_text(tl!("Create a new data set from the current values")).clicked() {
            // As in Photoshop, the new data set starts from the one shown.
            let values = sets.get(cur).and_then(|s| s.get("values")).cloned().unwrap_or_else(|| json!([]));
            let name = unused_name(sets);
            sets.push(json!({"name": name, "values": values}));
            cur = sets.len() - 1;
        }
        if !sets.is_empty() && ui.button(tl!("Delete")).clicked() {
            sets.remove(cur);
            cur = cur.saturating_sub(1);
        }
    });
    ui.separator();

    if let Some(set) = sets.get_mut(cur) {
        // name
        let mut sname = set.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        ui.horizontal(|ui| {
            ui.label(tl!("Name:"));
            if ui.add(egui::TextEdit::singleline(&mut sname).desired_width(200.0)).changed() {
                set["name"] = json!(sname);
            }
        });
        ui.add_space(4.0);
        let Some(values) = set.get_mut("values").and_then(Value::as_array_mut) else { return };
        for (vname, vty) in &def_meta {
            // find or create a value entry for this variable
            let idx = values.iter().position(|v| v.get("variable").and_then(Value::as_str) == Some(vname));
            let i = match idx {
                Some(i) => i,
                None => {
                    let default = match vty.as_str() {
                        "visibility" => json!({"variable": vname, "kind": "visibility", "value": true}),
                        "pixelReplacement" => json!({"variable": vname, "kind": "pixels", "value": ""}),
                        _ => json!({"variable": vname, "kind": "text", "value": ""}),
                    };
                    values.push(default);
                    values.len() - 1
                }
            };
            ui.horizontal(|ui| {
                ui.add_sized([130.0, 18.0], egui::Label::new(vname.as_str()).truncate());
                match vty.as_str() {
                    "visibility" => {
                        let mut on = values[i].get("value").and_then(Value::as_bool).unwrap_or(true);
                        if ui.checkbox(&mut on, tl!("Visible")).changed() {
                            values[i]["value"] = json!(on);
                        }
                    }
                    "pixelReplacement" => {
                        let mut p = values[i].get("value").and_then(Value::as_str).unwrap_or("").to_string();
                        // Empty is Photoshop's "Do Not Replace".
                        if ui.add(egui::TextEdit::singleline(&mut p).hint_text(tl!("Do Not Replace")).desired_width(200.0)).changed() {
                            values[i]["value"] = json!(p);
                        }
                        if ui.button(tl!("Choose…")).on_hover_text(tl!("Select an image file")).clicked() {
                            requests.pick_image = Some(vname.clone());
                        }
                    }
                    _ => {
                        let mut txt = values[i].get("value").and_then(Value::as_str).unwrap_or("").to_string();
                        if ui.add(egui::TextEdit::singleline(&mut txt).desired_width(240.0)).changed() {
                            values[i]["value"] = json!(txt);
                        }
                    }
                }
            });
        }
        ui.add_space(6.0);
        if crate::widgets::secondary_button(ui, tl!("Apply"), 84.0).clicked() {
            fields.insert("__apply".into(), json!(sname));
        }
    }
    ui.add_space(6.0);
    if let Some(i) = data_set_table(ui, "vars-table", &def_meta, sets, cur) {
        cur = i;
    }
    // Photoshop's Import Data Set options, then the file.
    ui.horizontal(|ui| {
        if ui.button(tl!("Import…")).on_hover_text(tl!("Import data sets from a comma- or tab-delimited text file")).clicked() {
            requests.import = true;
        }
        for (key, label) in [("__importFirstColumn", tl!("Use First Column For Data Set Names")), ("__importReplace", tl!("Replace Existing Data Sets"))] {
            let mut on = fields.get(key).and_then(Value::as_bool).unwrap_or(true);
            if ui.checkbox(&mut on, label).changed() {
                fields.insert(key.into(), json!(on));
            }
        }
    });
    fields.insert("__cur".into(), json!(cur));
}

/// `Data Set N` for the lowest N no data set uses.
fn unused_name(sets: &[Value]) -> String {
    let taken = |n: usize| sets.iter().any(|s| s.get("name").and_then(Value::as_str) == Some(&format!("Data Set {n}")));
    let n = (1..).find(|&n| !taken(n)).unwrap_or(sets.len() + 1);
    format!("Data Set {n}")
}

/// One cell of the data-set table.
fn cell_text(set: &Value, var: &str) -> String {
    let value = set.get("values").and_then(Value::as_array).and_then(|vs| vs.iter().find(|v| v.get("variable").and_then(Value::as_str) == Some(var)));
    match value.and_then(|v| v.get("value")) {
        Some(Value::Bool(true)) => tl!("Visible").to_string(),
        Some(Value::Bool(false)) => tl!("Invisible").to_string(),
        Some(Value::String(s)) if s.is_empty() => "—".into(),
        Some(Value::String(s)) => s.clone(),
        _ => "—".into(),
    }
}

/// Photoshop's table of every data set (rows) by variable (columns). Returns the clicked row.
fn data_set_table(ui: &mut egui::Ui, id: &str, defs: &[(String, String)], sets: &[Value], cur: usize) -> Option<usize> {
    let mut clicked = None;
    egui::ScrollArea::both().id_salt(id).max_height(160.0).show(ui, |ui| {
        egui::Grid::new(id).striped(true).num_columns(defs.len() + 1).spacing([12.0, 4.0]).show(ui, |ui| {
            ui.strong(tl!("Data Set"));
            for (name, _) in defs {
                ui.strong(name.as_str());
            }
            ui.end_row();
            for (i, set) in sets.iter().enumerate() {
                let name = set.get("name").and_then(Value::as_str).unwrap_or_default();
                if ui.selectable_label(i == cur, name).clicked() {
                    clicked = Some(i);
                }
                for (var, _) in defs {
                    ui.add_sized([120.0, 18.0], egui::Label::new(cell_text(set, var)).truncate());
                }
                ui.end_row();
            }
        });
    });
    clicked
}

/// Image › Apply Data Set…: pick a data set from the list; OK applies it.
fn apply_page(ui: &mut egui::Ui, state: &mut Value, fields: &mut Map<String, Value>) {
    let defs: Vec<(String, String)> = state
        .get("defs")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|d| (d.get("name").and_then(Value::as_str).unwrap_or("").to_string(), String::new())).collect())
        .unwrap_or_default();
    let sets = state.get("dataSets").and_then(Value::as_array).cloned().unwrap_or_default();
    if sets.is_empty() {
        ui.weak(tl!("This document has no data sets. Define them in Image › Variables › Data Sets."));
        fields.remove("__apply");
        return;
    }
    let mut cur = data_set_index(fields, sets.len());
    if let Some(i) = data_set_table(ui, "apply-table", &defs, &sets, cur) {
        cur = i;
    }
    let name = sets.get(cur).and_then(|s| s.get("name")).and_then(Value::as_str).unwrap_or_default().to_string();
    fields.insert("__cur".into(), json!(cur));
    fields.insert("__apply".into(), json!(name));
}

/// The dialog state ready to save: data-set values only for variables still defined (a renamed
/// or deleted variable leaves none behind), and an error for an empty or repeated data-set name.
fn prepared_sets(state: &Value) -> Result<Value, String> {
    let defined: Vec<&str> = state.get("defs").and_then(Value::as_array).into_iter().flatten().filter_map(|d| d.get("name").and_then(Value::as_str)).collect();
    let mut names: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for set in state.get("dataSets").and_then(Value::as_array).into_iter().flatten() {
        let name = set.get("name").and_then(Value::as_str).unwrap_or_default().trim().to_string();
        if name.is_empty() {
            return Err(tl!("A data set needs a name").to_string());
        }
        if names.iter().any(|n| n == &name) {
            let template = "Two data sets are named \"{name}\"";
            return Err(crate::i18n::fmt(tl!(template), &[("name", &name)]));
        }
        let values: Vec<Value> = set
            .get("values")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|v| v.get("variable").and_then(Value::as_str).is_some_and(|n| defined.contains(&n)))
            .cloned()
            .collect();
        out.push(json!({"name": name, "values": values}));
        names.push(name);
    }
    Ok(Value::Array(out))
}

/// OK: save the definitions and data sets, and apply one if the Apply button set `__apply`.
pub fn confirm(app: &mut PhotocraftApp, fields: &Map<String, Value>) -> Result<Value, String> {
    if fields.get("__page").and_then(Value::as_str) == Some("apply") {
        let name = fields.get("__apply").and_then(Value::as_str).ok_or_else(|| tl!("This document has no data sets").to_string())?;
        return app.run("image.applyDataSet", json!({"name": name}));
    }
    let state = normalize(fields.get("__variables").unwrap_or(&Value::Null));
    let defs = state.get("defs").cloned().unwrap_or_else(|| json!([]));
    let sets = prepared_sets(&state)?;
    // Only define when there is at least one named variable (empty define clears).
    app.run("image.variables.define", json!({"defs": defs}))?;
    app.run("image.variables.dataSets", json!({"dataSets": sets}))?;
    if let Some(name) = fields.get("__apply").and_then(Value::as_str) {
        return app.run("image.applyDataSet", json!({"name": name}));
    }
    Ok(json!({"defined": true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::{DataSet, DataValue, LayerId, VarKind, VariableDef, Variables, variables::Value as VV};

    #[test]
    fn to_state_mirrors_the_command_shape() {
        let v = Variables {
            defs: vec![
                VariableDef { name: "show".into(), layer: LayerId(1), kind: VarKind::Visibility },
                VariableDef { name: "title".into(), layer: LayerId(2), kind: VarKind::TextReplacement },
            ],
            data_sets: vec![DataSet { name: "A".into(), values: vec![DataValue { variable: "show".into(), value: VV::Visibility(false) }] }],
            active: None,
        };
        let s = to_state(&v);
        assert_eq!(s["defs"][0]["name"], "show");
        assert_eq!(s["defs"][0]["type"], "visibility");
        assert_eq!(s["defs"][1]["type"], "textReplacement");
        assert_eq!(s["dataSets"][0]["name"], "A");
        assert_eq!(s["dataSets"][0]["values"][0]["kind"], "visibility");
        assert_eq!(s["dataSets"][0]["values"][0]["value"], false);
    }

    #[test]
    fn data_set_index_rejects_out_of_range_control_values() {
        let mut fields = Map::new();
        assert_eq!(data_set_index(&fields, 1), 0);
        fields.insert("__cur".into(), json!(999));
        assert_eq!(data_set_index(&fields, 1), 0);
        fields.insert("__cur".into(), json!(u64::MAX));
        assert_eq!(data_set_index(&fields, 1), 0);
        fields.insert("__cur".into(), json!(1));
        assert_eq!(data_set_index(&fields, 3), 1);
        assert_eq!(data_set_index(&fields, 0), 0);
    }

    #[test]
    fn normalize_keeps_only_object_entries() {
        let s = normalize(&json!({"defs": [1, {"name": "a"}], "dataSets": ["x", {"name": "A", "values": [true, {"variable": "a"}]}, {"values": 3}]}));
        assert_eq!(s, json!({"defs": [{"name": "a"}], "dataSets": [{"name": "A", "values": [{"variable": "a"}]}, {"values": []}]}));
        for bad in [Value::Null, json!(5), json!("s"), json!([1]), json!({"defs": "x", "dataSets": {}})] {
            assert_eq!(normalize(&bad), json!({"defs": [], "dataSets": []}), "{bad}");
        }
    }

    #[test]
    fn alt_reset_restores_open_snapshot() {
        let mut f = Map::new();
        let orig = json!({"defs": [{"name": "a"}], "dataSets": []});
        f.insert("__variables".into(), json!({"defs": [{"name": "edited"}], "dataSets": []}));
        f.insert("__variablesOrig".into(), orig.clone());
        reset(&mut f);
        assert_eq!(f["__variables"], orig);
    }

    #[test]
    fn layer_kinds_offer_photoshops_variable_types() {
        assert!(LayerKind::Text.allows("textReplacement") && !LayerKind::Text.allows("pixelReplacement"));
        assert!(LayerKind::Pixel.allows("pixelReplacement") && !LayerKind::Pixel.allows("textReplacement"));
        assert!(!LayerKind::Other.allows("textReplacement") && !LayerKind::Other.allows("pixelReplacement"));
        assert!([LayerKind::Text, LayerKind::Pixel, LayerKind::Other].iter().all(|k| k.allows("visibility")));
    }

    #[test]
    fn saving_drops_stale_values_and_rejects_repeated_names() {
        let state = json!({
            "defs": [{"name": "kept"}],
            "dataSets": [{"name": " A ", "values": [{"variable": "kept", "kind": "text", "value": "x"}, {"variable": "renamedAway", "kind": "text", "value": "y"}]}],
        });
        assert_eq!(prepared_sets(&state).unwrap(), json!([{"name": "A", "values": [{"variable": "kept", "kind": "text", "value": "x"}]}]));
        assert!(prepared_sets(&json!({"defs": [], "dataSets": [{"name": "A"}, {"name": "A"}]})).is_err());
        assert!(prepared_sets(&json!({"defs": [], "dataSets": [{"name": "  "}]})).is_err());
    }

    #[test]
    fn new_data_set_names_skip_used_ones() {
        assert_eq!(unused_name(&[json!({"name": "Data Set 1"}), json!({"name": "Data Set 3"})]), "Data Set 2");
        assert_eq!(unused_name(&[]), "Data Set 1");
    }

    #[test]
    fn apply_data_set_menu_opens_a_picker_that_applies_the_chosen_set() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 40, "height": 30})).unwrap();
        let layer = app.session.active().unwrap().active_layer.unwrap().0;
        app.run("image.variables.define", json!({"defs": [{"name": "show", "layer": layer, "type": "visibility"}]})).unwrap();
        let set = |name: &str, on: bool| json!({"name": name, "values": [{"variable": "show", "kind": "visibility", "value": on}]});
        app.run("image.variables.dataSets", json!({"dataSets": [set("on", true), set("off", false)]})).unwrap();
        let d = menu(&mut app, "image.applyDataSet", &json!({})).unwrap().unwrap()["dialog"].as_u64().unwrap();
        let fields = &mut app.ui.dialog_mut(d).unwrap().fields;
        assert_eq!(fields["__page"], "apply");
        // What the list sets when a row is clicked.
        fields.insert("__cur".into(), json!(1));
        fields.insert("__apply".into(), json!("off"));
        crate::dialogs::confirm(&mut app, d).unwrap();
        let st = app.session.active().unwrap();
        assert_eq!(st.doc.variables.active, Some(1));
        assert!(!st.doc.layer(photocraft_doc::LayerId(layer)).unwrap().visible);
    }

    type Open = std::rc::Rc<std::cell::RefCell<Vec<(FileDialogRequest, crate::file_dialog::FileDialogReply)>>>;

    /// An app whose file dialogs wait for the test, with a document holding a type layer
    /// ("title") and a pixel layer ("photo") bound to variables, and one data set.
    fn template() -> (PhotocraftApp, Open, egui::Context, u64, u64) {
        let open: Open = Default::default();
        let held = open.clone();
        let services = crate::Services { file_dialog: Some(Box::new(move |request, _, reply| held.borrow_mut().push((request, reply)))), ..Default::default() };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.run("file.new", json!({"width": 40, "height": 30})).unwrap();
        let photo = app.run("layer.new.layer", json!({"name": "photo"})).unwrap()["layer"].as_u64().unwrap();
        app.run("type.create", json!({"x": 2, "y": 20, "text": "Old", "size": 12})).unwrap();
        let title = app.session.active().unwrap().active_layer.unwrap().0;
        let defs = json!([{"name": "title", "layer": title, "type": "textReplacement"}, {"name": "photo", "layer": photo, "type": "pixelReplacement"}]);
        app.run("image.variables.define", json!({"defs": defs})).unwrap();
        app.run("image.variables.dataSets", json!({"dataSets": [{"name": "A", "values": [{"variable": "title", "kind": "text", "value": "New"}]}]})).unwrap();
        (app, open, egui::Context::default(), title, photo)
    }

    fn answer(app: &mut PhotocraftApp, ctx: &egui::Context, open: &Open, answer: FileDialogAnswer) {
        app.poll_file_dialog(ctx, None);
        let (_, reply) = open.borrow_mut().pop().expect("a dialog is open");
        std::thread::spawn(move || reply.send(Some(answer))).join().unwrap();
        app.poll_file_dialog(ctx, None);
    }

    fn sets(app: &mut PhotocraftApp, d: u64) -> Value {
        app.ui.dialog_mut(d).unwrap().fields["__variables"]["dataSets"].clone()
    }

    #[test]
    fn import_and_choose_fill_the_dialog_not_the_document() {
        let (mut app, open, ctx, _, _) = template();
        let d = menu(&mut app, "image.variables.dataSets", &json!({})).unwrap().unwrap()["dialog"].as_u64().unwrap();
        // Defaults: first column names the sets, replacing the dialog's.
        import(&mut app, d).unwrap();
        answer(&mut app, &ctx, &open, FileDialogAnswer::Contents("sets.tsv".into(), b"name\ttitle\nB\tHello\n".to_vec()));
        assert_eq!(sets(&mut app, d), json!([{"name": "B", "values": [{"variable": "title", "kind": "text", "value": "Hello"}]}]));
        app.ui.dialog_mut(d).unwrap().fields.insert("__importReplace".into(), json!(false));
        import(&mut app, d).unwrap();
        answer(&mut app, &ctx, &open, FileDialogAnswer::Contents("sets.csv".into(), b"name,title\nB,Again\n".to_vec()));
        let names: Vec<Value> = sets(&mut app, d).as_array().unwrap().iter().map(|s| s["name"].clone()).collect();
        assert_eq!(names, [json!("B"), json!("B 2")]);
        assert_eq!(app.ui.dialog_mut(d).unwrap().fields["__cur"], 1, "the first imported set is shown");

        pick_image(&mut app, d, 1, "photo".into()).unwrap();
        app.poll_file_dialog(&ctx, None);
        assert!(matches!(open.borrow().as_slice(), [(FileDialogRequest::Open { multiple: false, extensions: Some(e), .. }, _)] if e.iter().any(|x| x == "png")));
        // Cancelled: the value stays empty.
        let (_, reply) = open.borrow_mut().pop().unwrap();
        std::thread::spawn(move || reply.send(None)).join().unwrap();
        app.poll_file_dialog(&ctx, None);
        assert!(!sets(&mut app, d)[1]["values"].as_array().unwrap().iter().any(|v| v["variable"] == "photo"));
        pick_image(&mut app, d, 1, "photo".into()).unwrap();
        answer(&mut app, &ctx, &open, FileDialogAnswer::Paths(vec!["/pics/a.png".into()]));
        assert!(sets(&mut app, d)[1]["values"].as_array().unwrap().contains(&json!({"variable": "photo", "kind": "pixels", "value": "/pics/a.png"})));
        // Nothing reached the document until OK.
        let doc_sets: Vec<String> = app.session.active().unwrap().doc.variables.data_sets.iter().map(|s| s.name.clone()).collect();
        assert_eq!(doc_sets, ["A"]);
        crate::dialogs::confirm(&mut app, d).unwrap();
        assert_eq!(app.session.active().unwrap().doc.variables.data_sets.len(), 2);
    }

    #[test]
    fn preview_shows_the_set_without_changing_the_document() {
        let (mut app, _open, _ctx, title, _) = template();
        let d = menu(&mut app, "image.applyDataSet", &json!({})).unwrap().unwrap()["dialog"].as_u64().unwrap();
        let text = |doc: &photocraft_doc::Document| match &doc.layer(photocraft_doc::LayerId(title)).unwrap().content {
            photocraft_doc::LayerContent::Text(t) => t.text.clone(),
            _ => String::new(),
        };
        let (shown, key) = crate::variables_preview::display_doc(&mut app, 0).expect("Apply Data Set previews by default");
        assert_eq!(text(&shown), "New");
        assert_eq!(text(&app.session.active().unwrap().doc), "Old");
        assert_eq!(crate::variables_preview::display_doc(&mut app, 0).unwrap().1, key, "cached while nothing changes");
        // A bad image path: no preview, and the dialog says why.
        let fields = &mut app.ui.dialog_mut(d).unwrap().fields;
        fields["__variables"]["dataSets"][0]["values"] = json!([{"variable": "photo", "kind": "pixels", "value": "/no/such/image.png"}]);
        assert!(crate::variables_preview::display_doc(&mut app, 0).is_none());
        assert!(crate::variables_preview::error(&app, d).is_some());
        app.ui.dialog_mut(d).unwrap().fields.insert("__preview".into(), json!(false));
        assert!(crate::variables_preview::display_doc(&mut app, 0).is_none(), "Preview off");
        app.ui.close_dialog(d);
        assert!(crate::variables_preview::display_doc(&mut app, 0).is_none());
        assert!(app.variables_preview.is_none(), "closing drops the preview");
    }

    #[test]
    fn set_value_replaces_or_adds_and_ignores_missing_sets() {
        let mut s = json!({"dataSets": [{"values": [{"variable": "a", "value": 1}]}]});
        set_value(&mut s, 0, "a", json!({"variable": "a", "value": 2}));
        set_value(&mut s, 0, "b", json!({"variable": "b", "value": 3}));
        set_value(&mut s, 9, "c", json!({"variable": "c"}));
        assert_eq!(s, json!({"dataSets": [{"values": [{"variable": "a", "value": 2}, {"variable": "b", "value": 3}]}]}));
    }

    #[test]
    fn owns_detects_the_field() {
        let mut f = Map::new();
        assert!(!owns(&f));
        f.insert("__variables".into(), json!({}));
        assert!(owns(&f));
    }
}
