//! Argument schemas of the agent tools: a type, a description and (where there is a fixed set of
//! values) an enum for every property, and the check that holds each call to its schema, so a
//! typo or a value outside the set is an error that says what was meant instead of being ignored.

use serde_json::{json, Map, Value};

use super::R;

/// Views of `render`, `render_assembly`, `render_cam`, `simulate_cam` and `render_simulation`.
pub(super) const VIEWS: [&str; 15] = [
    "iso",
    "iso_front_right",
    "iso_front_left",
    "iso_back_right",
    "iso_back_left",
    "iso_front_right_below",
    "iso_front_left_below",
    "iso_back_right_below",
    "iso_back_left_below",
    "front",
    "back",
    "top",
    "bottom",
    "left",
    "right",
];

/// Feature kinds `add_feature` takes (every kind `Op::new_default` knows).
pub(super) const FEATURE_KINDS: [&str; 37] = [
    "extrude",
    "revolve",
    "sweep",
    "loft",
    "fillet",
    "chamfer",
    "shell",
    "hole",
    "draft",
    "thicken",
    "rib",
    "moveFace",
    "deleteFace",
    "helix",
    "plane",
    "linearPattern",
    "circularPattern",
    "curvePattern",
    "pointPattern",
    "mirror",
    "boolean",
    "split",
    "transform",
    "deleteBody",
    "sheetMetal",
    "heal",
    "custom",
    "derived",
    "flange",
    "tab",
    "convertSheet",
    "surfaceExtrude",
    "surfaceRevolve",
    "surfaceLoft",
    "fill",
    "offsetSurface",
    "knit",
];

/// Tools that change the active Part Studio: a call that leaves a feature failing reports
/// `ok: false` with `new_errors`, and `rollback: true` undoes it.
pub(super) const MODELLING_TOOLS: [&str; 11] = [
    "set_variable",
    "create_sketch",
    "sketch_add",
    "sketch_constrain",
    "add_feature",
    "edit_feature",
    "delete_feature",
    "import_file",
    "define_custom_feature",
    "set_part",
    "set_parts",
];

// ---------------------------------------------------------------- schema builders

fn t(ty: &str, desc: &str) -> Value {
    json!({"type": ty, "description": desc})
}

fn en(values: &[&str], desc: &str) -> Value {
    json!({"type": "string", "enum": values, "description": desc})
}

fn arr(items: Value, desc: &str) -> Value {
    json!({"type": "array", "items": items, "description": desc})
}

fn nums(n: usize, desc: &str) -> Value {
    json!({"type": "array", "items": {"type": "number"}, "minItems": n, "maxItems": n, "description": desc})
}

fn xyz(desc: &str) -> Value {
    nums(3, desc)
}

fn xy(desc: &str) -> Value {
    nums(2, desc)
}

fn body_id(desc: &str) -> Value {
    json!({"type": "array", "items": {"type": "integer", "minimum": 0}, "minItems": 2, "maxItems": 2, "description": desc})
}

fn obj(props: Value, req: &[&str], desc: &str) -> Value {
    json!({"type": "object", "properties": props, "required": req, "additionalProperties": false, "description": desc})
}

/// The persistent name a face or edge ref carries once a feature stored it (as describe shows it).
fn topo_name() -> Value {
    arr(
        obj(json!({"feature": t("integer", "feature id"), "role": t("string", "face role"), "n": t("integer", "index")}), &["feature", "role"], "a face name"),
        "persistent name of the face, as describe shows a stored ref (optional; the point picks)",
    )
}

fn face_pick(desc: &str) -> Value {
    obj(
        json!({"kind": en(&["face"], "optional, as list_geometry gives it"), "point": xyz("a point on the face, mm"), "normal": xyz("the face's outward normal"), "name": topo_name()}),
        &["point"],
        desc,
    )
}

fn view() -> Value {
    en(&VIEWS, "standard or isometric view (iso = iso_front_right)")
}

fn width(default: u32) -> Value {
    json!({"type": "integer", "minimum": 64, "maximum": 4096, "description": format!("image width, px (default {default})")})
}

fn height(default: u32) -> Value {
    json!({"type": "integer", "minimum": 64, "maximum": 4096, "description": format!("image height, px (default {default})")})
}

fn out_path(what: &str) -> Value {
    t("string", &format!("{what}; relative paths are from the working directory, missing folders are created"))
}

fn materials() -> Vec<&'static str> {
    crate::doc::MATERIALS.iter().map(|m| m.0).collect()
}

fn fea_materials() -> Vec<&'static str> {
    std::iter::once("").chain(crate::fea::MATERIALS.iter().map(|m| m.0)).collect()
}

fn cam_keys<T: Copy>(all: &[T], key: fn(T) -> &'static str) -> Vec<&'static str> {
    all.iter().map(|k| key(*k)).collect()
}

/// A property for each serialized field of a CAM struct that the tool overlays onto it.
fn serde_fields<T: serde::Serialize>(v: &T, skip: &[&str], what: &str) -> Map<String, Value> {
    let mut out = Map::new();
    let Ok(Value::Object(m)) = serde_json::to_value(v) else { return out };
    for (k, x) in m {
        if skip.contains(&k.as_str()) {
            continue;
        }
        let ty = match &x {
            Value::Bool(_) => json!("boolean"),
            Value::Number(n) if n.is_f64() => json!("number"),
            Value::Number(_) => json!("integer"),
            Value::String(_) => json!("string"),
            Value::Null => json!(["number", "null"]),
            Value::Array(_) => json!("array"),
            Value::Object(_) => json!("object"),
        };
        let mut p = json!({"type": ty, "description": format!("{what} `{k}` (default {x})")});
        if let Value::Array(a) = &x {
            p["items"] = match a.first() {
                Some(Value::Number(_)) => json!({"type": "number"}),
                Some(Value::String(_)) => json!({"type": "string"}),
                _ => json!({"type": "number"}),
            };
        }
        out.insert(k, p);
    }
    out
}

// ---------------------------------------------------------------- property catalogue

/// The full schema of a property that several tools share.
fn common(prop: &str) -> Option<Value> {
    Some(match prop {
        "path" => out_path("file path"),
        "name" => t("string", "display name"),
        "studio" => json!({"type": "integer", "minimum": 0, "description": "Part Studio index (describe lists them); default the active one"}),
        "body" => body_id("part id [feature, index] as describe and list_geometry give it"),
        "bodies" => arr(body_id("[feature, index]"), "parts [[feature, index], …]; default every visible part"),
        "cam" => json!({"type": "integer", "minimum": 0, "description": "Manufacture tab index (from create_cam)"}),
        "assembly" => json!({"type": "integer", "minimum": 0, "description": "Assembly tab index (from create_assembly)"}),
        "drawing" => json!({"type": "integer", "minimum": 0, "description": "Drawing tab index (from create_drawing)"}),
        "simulation" => json!({"type": "integer", "minimum": 0, "description": "Simulation tab index (from create_simulation)"}),
        "sketch_id" => json!({"type": "integer", "minimum": 1, "description": "the sketch's feature id (from create_sketch)"}),
        "view" => view(),
        "direction" => xyz("view direction from the part towards the camera (front is [0,-1,0]); overrides view"),
        "width" => width(800),
        "height" => height(600),
        "render" => t("boolean", "also return a PNG"),
        "rollback" => t("boolean", "undo the whole call when it leaves a feature failing (default false: the failing feature stays, and the result has ok:false and new_errors)"),
        "post" => en(&cam_keys(&crate::cam::POSTS, |p| p.0), "G-code dialect"),
        "delete" => t("boolean", "delete it instead"),
        "hidden" => t("boolean", "hide it"),
        "fixed" => t("boolean", "fixed in place (mates move the other instances)"),
        "position" => xyz("position [x, y, z], mm"),
        "rotate" => obj(json!({"axis": xyz("rotation axis"), "angle": t("number", "degrees")}), &["axis", "angle"], "rotation about an axis through the origin"),
        "field" => en(&["stress", "max_principal", "min_principal", "displacement", "safety"], "colour map: von Mises stress (default), max or min principal stress, displacement, or factor of safety"),
        "mode" => json!({"type": "integer", "minimum": 1, "description": "modal: which mode, 1-based"}),
        "time" => t("number", "transient: time, s (default the moment of largest displacement)"),
        "deform" => t("number", "deformation scale × the automatic exaggeration (0 = undeformed)"),
        "analysis" => en(&["static", "modal", "transient"], "static: deflection and stress; modal: natural frequencies; transient: response over time"),
        "material" => t("string", "material name"),
        "kind" => t("string", "kind"),
        "id" => json!({"type": "integer", "minimum": 0, "description": "id"}),
        _ => return None,
    })
}

/// The schema of one tool's property, where it differs from the shared one.
fn specific(tool: &str, prop: &str) -> Option<Value> {
    let fields = |props: Value, req: &[&str], desc: &str| obj(props, req, desc);
    Some(match (tool, prop) {
        ("new_document", "units") => en(&["mm", "cm", "m", "in", "ft"], "document units (default mm)"),
        ("open_document", "path") => t("string", "the .json document to open"),
        ("save_document", "path") => out_path("where to save the .json document (default where it was opened or last saved)"),
        ("set_variable", "name") => t("string", "variable name: a letter or _, then letters, digits or _"),
        ("set_variable", "expr") => json!({"type": ["string", "number"], "description": "value: a number or an expression of earlier variables, e.g. '40', 'w / 2', '1 in'"}),
        ("create_sketch", "plane") => json!({"oneOf": [
            en(&["top", "front", "right", "xy", "xz", "yz"], "an origin plane"),
            json!({"type": "object", "description": "a planar face ref {kind:'face', point, normal} from list_geometry, or {kind:'feature_plane', feature: id}", "properties": {"kind": en(&["face", "feature_plane", "origin_plane"], "ref kind"), "point": xyz("point on the face"), "normal": xyz("face normal"), "feature": t("integer", "plane feature id"), "plane": en(&["Top", "Front", "Right"], "origin plane"), "name": topo_name()}, "required": ["kind"], "additionalProperties": false})
        ], "description": "the sketch plane (default top)"}),
        ("create_sketch", "name") => t("string", "sketch name (default Sketch N)"),
        ("sketch_add", "items") => arr(
            json!({"type": "object", "description": "one item; the keys it takes depend on type (see the tool description)", "required": ["type"], "additionalProperties": false, "properties": {
                "type": en(&["point", "line", "polyline", "rect", "center_rect", "circle", "arc", "ellipse", "elliptical_arc", "spline", "polygon", "text"], "item type"),
                "construction": t("boolean", "construction geometry (not part of regions)"),
                "at": xy("point / text anchor [x, y]"), "from": xy("line start"), "to": xy("line end"),
                "points": arr(xy("[x, y]"), "polyline or spline points"), "closed": t("boolean", "close the polyline or spline"),
                "corner1": xy("rect corner"), "corner2": xy("opposite rect corner"), "center": xy("centre [x, y]"), "corner": xy("center_rect corner"),
                "radius": json!({"type": "number", "exclusiveMinimum": 0, "description": "circle / polygon radius, mm"}),
                "start": xy("arc start (counter-clockwise from start to end)"), "end": xy("arc end"),
                "major_end": xy("end of the ellipse's major axis"), "minor_radius": json!({"type": "number", "exclusiveMinimum": 0, "description": "ellipse minor radius"}),
                "start_angle": t("number", "elliptical arc start, degrees from the major axis"), "end_angle": t("number", "elliptical arc end, degrees"),
                "sides": json!({"type": "integer", "minimum": 3, "description": "polygon sides (default 6)"}),
                "text": t("string", "text to write"), "height": json!({"type": "number", "exclusiveMinimum": 0, "description": "text height, mm"})
            }}),
            "items to add",
        ),
        ("sketch_constrain", "constraints") => arr(
            json!({"type": "object", "required": ["type"], "additionalProperties": false, "description": "one constraint by point and curve ids (see the tool description)", "properties": {
                "type": en(&["coincident", "point_on_line", "point_on_circle", "horizontal", "vertical", "horizontal_pts", "vertical_pts", "parallel", "perpendicular", "tangent", "equal", "midpoint", "symmetric", "fix", "distance", "axis_distance", "point_line_distance", "length", "radius", "diameter", "angle", "equal_distance"], "constraint type"),
                "a": t("integer", "first point or curve id"), "b": t("integer", "second point or curve id"), "p": t("integer", "point id"), "l": t("integer", "line id"), "c": t("integer", "circle or arc id"),
                "q": t("integer", "equal_distance: second point id"), "m": t("integer", "equal_distance: second line id"),
                "driven": t("boolean", "dimensions: a reference that measures and drives nothing"),
                "x": t("number", "fix: x, mm"), "y": t("number", "fix: y, mm"), "axis": json!({"type": "integer", "enum": [0, 1], "description": "axis_distance: 0 horizontal, 1 vertical"}),
                "inner": t("boolean", "tangent: internal tangency of circles"),
                "value": json!({"type": ["number", "string"], "description": "dimension value or expression (lengths in document units, angles in degrees)"}),
                "dim": json!({"type": "object", "description": "dimension in the document format {expr, offset}", "properties": {"expr": t("string", "expression"), "offset": xy("label offset"), "driven": t("boolean", "reference dimension")}, "additionalProperties": false})
            }}),
            "constraints to add",
        ),
        ("add_feature", "kind") => en(&FEATURE_KINDS, "feature kind; feature_defaults lists each kind's params and defaults"),
        ("add_feature", "params") => {
            json!({"type": "object", "additionalProperties": true, "description": "params overriding the kind's defaults (feature_defaults shows them); unknown names are rejected"})
        }
        ("add_feature", "name") => t("string", "feature name (default Kind N)"),
        ("edit_feature", "id") => json!({"type": "integer", "minimum": 1, "description": "feature id"}),
        ("edit_feature", "params") => json!({"type": "object", "additionalProperties": true, "description": "params to change (as in add_feature)"}),
        ("edit_feature", "suppressed") => t("boolean", "suppress or unsuppress the feature"),
        ("delete_feature", "id") => json!({"type": "integer", "minimum": 1, "description": "feature id"}),
        ("feature_defaults", "kind") => en(&FEATURE_KINDS, "a feature kind; leave out to list the kinds"),
        ("import_file", "path") => t("string", "file to import (.step, .stp, .iges, .igs, .stl, .obj, .dxf, .svg); relative paths are also looked for next to a `run` script"),
        ("import_file", "scale") => json!({"type": ["number", "string"], "exclusiveMinimum": 0, "description": "scale factor, e.g. 25.4 for a file in inches (default 1)"}),
        ("import_file", "merge_faces") => t("boolean", "meshes: join coplanar triangles into flat faces (default true)"),
        ("import_file", "name") => t("string", "feature (or new sketch) name; default the file name"),
        ("import_file", "sketch_id") => json!({"type": "integer", "minimum": 1, "description": "DXF/SVG: add to this sketch (default a new one)"}),
        ("import_file", "plane") => specific("create_sketch", "plane").unwrap(),
        ("import_file", "at") => xy("DXF/SVG: offset [x, y] in the sketch, mm"),
        ("printability", "overhang") => json!({"type": "number", "minimum": 0, "maximum": 90, "description": "overhang limit, degrees from vertical (default 45)"}),
        ("printability", "min_wall") => json!({"type": "number", "exclusiveMinimum": 0, "description": "thinnest printable wall, mm (default 0.8)"}),
        ("send_to_slicer", "url") => t("string", "orcaweb address (default the app's setting)"),
        ("send_to_slicer", "tolerance") => json!({"type": "number", "exclusiveMinimum": 0, "description": "STL tolerance, mm (default 0.01)"}),
        ("send_to_slicer", "name") => t("string", "job name (default the document's)"),
        ("set_part" | "set_parts", "color") => {
            json!({"oneOf": [json!({"type": "string", "description": "#rrggbb"}), json!({"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}, "minItems": 3, "maxItems": 3, "description": "[r, g, b]"})], "description": "display colour"})
        }
        ("set_part" | "set_parts", "material") => en(&materials(), "material (gives mass in the BOM and mass_properties)"),
        ("set_part" | "set_parts", "finish") => en(&["default", "matte", "satin", "glossy", "metal", "glass", ""], "surface finish"),
        ("set_part" | "set_parts", "name") => t("string", "part name (shown by describe, the BOM and exports)"),
        ("set_parts", "parts") => arr(
            obj(
                json!({"body": common("body").unwrap(), "name": t("string", "part name"), "color": specific("set_part", "color").unwrap(), "material": specific("set_part", "material").unwrap(), "finish": specific("set_part", "finish").unwrap(), "hidden": t("boolean", "hide the part")}),
                &["body"],
                "one part's settings",
            ),
            "the parts to set",
        ),
        ("add_decal", "image") => t("string", "PNG or JPEG file"),
        ("add_decal", "face") => face_pick("a planar face ref; the image is centred on its point"),
        ("add_decal", "width") => json!({"type": "number", "exclusiveMinimum": 0, "description": "width, mm (default 20)"}),
        ("add_decal", "angle") => t("number", "turn about the face normal, degrees"),
        ("list_geometry", "type") => en(&["all", "faces", "edges"], "what to list (default all)"),
        ("list_geometry", "body") => body_id("only this part [feature, index]"),
        ("list_geometry", "box") => {
            json!({"type": "array", "items": xyz("corner"), "minItems": 2, "maxItems": 2, "description": "only refs whose point is inside [[xmin,ymin,zmin],[xmax,ymax,zmax]]"})
        }
        ("list_geometry", "normal") => xyz("only faces facing this way"),
        ("list_geometry", "planar") => t("boolean", "only planar (true) or curved (false) faces"),
        ("render", "sketches") => en(&["unused", "all", "none"], "which sketches to draw (default unused: those no feature consumed)"),
        ("render" | "render_assembly" | "render_cam" | "render_drawing" | "render_simulation" | "simulate_cam", "path") => out_path("also save the PNG here"),
        ("export", "format") => en(&["step", "iges", "stl", "obj"], "file format"),
        ("export", "path") => out_path("output file"),
        ("export", "ascii") => t("boolean", "STL: text instead of binary"),
        ("export_drawing", "format") => en(&["pdf", "svg", "dxf", "png"], "format (default from the path's extension)"),
        ("export_drawing", "path") => out_path("output file"),
        ("export_drawing", "sheet") => {
            json!({"oneOf": [en(&["A4", "A3", "A2", "A1", "Letter", "Tabloid"], "quick sheet size (without drawing; default A3)"), json!({"type": "integer", "minimum": 1, "description": "with drawing: sheet number for svg, dxf and png"})], "description": "sheet size, or with drawing the sheet number"})
        }
        ("export_drawing", "scale") => json!({"type": "number", "exclusiveMinimum": 0, "description": "quick sheet: view scale (default fits the sheet)"}),
        ("export_drawing", "title") => t("string", "quick sheet: title"),
        ("export_drawing" | "render_drawing", "width") => json!({"type": "integer", "minimum": 200, "maximum": 6000, "description": "png width, px (default 1600)"}),
        ("render_drawing", "sheet") => json!({"type": "integer", "minimum": 1, "description": "sheet number (default 1)"}),
        ("define_custom_feature", "name") => t("string", "feature type name (use it as params.name in add_feature kind custom)"),
        ("define_custom_feature", "params") => arr(
            obj(
                json!({"name": t("string", "parameter (a script variable)"), "kind": en(&["length", "angle", "number", "count", "bool"], "parameter kind (default length)"), "default": json!({"type": ["number", "string", "boolean"], "description": "default value or expression"})}),
                &["name"],
                "one parameter",
            ),
            "parameters",
        ),
        ("define_custom_feature", "selection") => en(&["", "points", "faces"], "what the feature takes as refs"),
        ("define_custom_feature", "source") => t("string", "Rhai source; its syntax is checked now"),
        ("new_part_studio", "name") => t("string", "name (default Part Studio N)"),
        ("switch_part_studio", "index") => json!({"type": "integer", "minimum": 0, "description": "Part Studio index"}),
        ("assembly_insert", "sub_assembly") => json!({"type": "integer", "minimum": 0, "description": "insert this assembly as a rigid sub-assembly instead of a part"}),
        ("assembly_insert", "body") => body_id("the part [feature, index] (default the studio's first part)"),
        ("assembly_mate" | "assembly_relation", "kind") => {
            if tool == "assembly_mate" {
                en(&["fastened", "revolute", "slider", "cylindrical", "planar", "ball"], "mate kind")
            } else {
                en(&["gear", "rack", "screw"], "relation kind")
            }
        }
        ("assembly_mate", "a" | "b") => obj(
            json!({"instance": t("integer", "instance id"), "face": xyz("a point on a face, in the part's own coordinates"), "edge": xyz("a point on an edge"), "origin": xyz("explicit connector origin"), "z": xyz("explicit connector Z axis"), "x": xyz("explicit connector X axis")}),
            &["instance"],
            "one side of the mate: instance plus face, edge or origin+z",
        ),
        ("assembly_mate", "flip") => t("boolean", "align the Z axes instead of opposing them"),
        ("assembly_mate", "offset") => t("number", "offset along Z, mm"),
        ("assembly_mate", "offset_x") => t("number", "offset along X, mm"),
        ("assembly_mate", "offset_y") => t("number", "offset along Y, mm"),
        ("assembly_mate", "rotation") => t("number", "rotation about Z, degrees"),
        ("assembly_mate", "limits") => xy("[min, max] of the free motion (mm or degrees)"),
        ("assembly_move", "instance") => t("integer", "instance id"),
        ("assembly_relation", "mate_a") => t("integer", "driving mate id"),
        ("assembly_relation", "mate_b") => t("integer", "driven mate id"),
        ("assembly_relation", "ratio") => t("number", "gear ratio, rack pitch radius (mm) or screw pitch (mm/turn)"),
        ("assembly_relation", "reverse") => t("boolean", "reverse the direction"),
        ("assembly_explode", "steps") => arr(
            obj(
                json!({"instances": arr(t("integer", "instance id"), "instances to move"), "direction": xyz("direction"), "distance": t("number", "mm (default 50)")}),
                &["instances"],
                "one explode step",
            ),
            "explode steps, applied in order",
        ),
        ("assembly_info", "interference") => t("boolean", "also list overlapping instances with their overlap volume"),
        ("export_web", "inline") => t("boolean", "one self-contained page (default true); false writes .json and .bin beside it"),
        ("export_web", "assembly") => json!({"type": "integer", "minimum": 0, "description": "the assembly to show (default: the parts of a Part Studio)"}),
        ("render_assembly", "explode") => json!({"type": "number", "minimum": 0, "maximum": 1, "description": "0..1 through the explode steps"}),
        ("create_drawing", "sheet") => en(&["A4", "A3", "A2", "A1", "Letter", "Tabloid"], "sheet size (default A3)"),
        ("create_drawing", "standard_views") => t("boolean", "add front, top, right and iso views (default true)"),
        ("drawing_add", "items") => arr(
            json!({"type": "object", "required": ["type"], "additionalProperties": false, "description": "one item; the keys it takes depend on type (see the tool description)", "properties": {
                "type": en(&["view", "section", "detail", "dimension", "auto_dimensions", "title", "note", "gdt", "datum", "bom", "sheet"], "item type"),
                "sheet": json!({"type": "integer", "minimum": 1, "description": "sheet number"}), "view": t("integer", "view id"), "studio": t("integer", "Part Studio index"),
                "body": body_id("view: one part of the studio (default all)"), "assembly": t("integer", "view: an assembly's instances at their poses; bom: assembly index"),
                "balloons": t("boolean", "assembly views: item balloons"), "parent": t("integer", "view: the parent view id"),
                "dir": t("string", "view direction: front|top|right|back|bottom|left|iso|section:front|section:top|section:right"),
                "through": xyz("section view: a model point the cut goes through"), "cut": t("number", "section view: cut offset, mm from the model centre"),
                "at": xy("section: a view point the cutting line goes through"), "line": en(&["horizontal", "vertical"], "section: cutting line direction"),
                "position": xy("sheet position, mm from the bottom-left"), "scale": t("number", "view scale"), "hidden_lines": t("boolean", "draw hidden lines"),
                "letter": t("string", "section, detail or datum letter"),
                "center": xy("view coordinates"), "radius": t("number", "mm"), "kind": t("string", "dimension kind (horizontal|vertical|aligned|radius|diameter|angle) or GD&T kind"),
                "a": xy("view coordinates"), "b": xy("view coordinates"), "line2": json!({"type": "array", "items": xy("[x, y]"), "minItems": 2, "maxItems": 2, "description": "angle: second line"}),
                "offset": t("number", "dimension line offset, mm"), "prefix": t("string", "dimension prefix, e.g. '4× '"), "suffix": t("string", "dimension suffix"),
                "tolerance": json!({"oneOf": [
                    t("string", "GD&T tolerance like 'Ø0.1', or the title block's general tolerance"),
                    t("number", "dimension: ± value"),
                    obj(json!({"plus": t("number", "upper deviation"), "minus": t("number", "lower deviation"), "upper": t("number", "same as plus"), "lower": t("number", "same as minus"), "limits": t("boolean", "show limit sizes")}), &[], "dimension: deviations")
                ], "description": "tolerance"}),
                "text": t("string", "note text"), "datums": t("string", "GD&T datums, e.g. 'A B'"), "leader": xy("leader end, sheet mm"),
                "part": t("string", "title block: part name"), "part_no": t("string", "title block: part number"), "material": t("string", "title block: material"),
                "revision": t("string", "title block: revision"), "date": t("string", "title block: date"), "drawn_by": t("string", "title block: drawn by"),
                "projection": en(&["first", "third"], "title block: projection angle"), "decimals": json!({"type": ["integer", "null"], "minimum": 0, "maximum": 6, "description": "title: decimals shown on dimensions (null = automatic)"}),
                "name": t("string", "title: the drawing's name")
            }}),
            "items to add",
        ),
        ("create_cam" | "edit_cam", "orient") => en(&crate::cam::ORIENTS, "the part side that faces the spindle"),
        ("create_cam" | "edit_cam", "origin") => en(&cam_keys(&crate::cam::Origin::ALL, crate::cam::Origin::key), "work zero"),
        ("create_cam" | "edit_cam", "tilt") => xy("3+2 tilt [about X, about Y], degrees"),
        ("create_cam" | "edit_cam", "axis") => xyz("part direction to point at the spindle (instead of orient and tilt)"),
        ("create_cam" | "edit_cam", "stock") => fields(
            json!({"side": t("number", "margin around the part, mm"), "top": t("number", "margin above, mm"), "bottom": t("number", "margin below, mm"), "size": json!({"type": ["array", "null"], "items": {"type": "number"}, "minItems": 3, "maxItems": 3, "description": "fixed size [x, y, z] instead of margins"}), "material": t("string", "stock material (Aluminium, Brass, Mild steel, Stainless steel, Plastic, Hardwood, Softwood / MDF, Foam)")}),
            &[],
            "stock margins or size, and material",
        ),
        ("create_cam" | "edit_cam", "machine") => json!({"oneOf": [
            en(&crate::cam::MACHINES.iter().map(|m| m.0).collect::<Vec<_>>(), "a machine preset"),
            obj(json!({"name": t("string", "name"), "travel": xyz("travel x, y, z, mm"), "max_feed": t("number", "mm/min"), "rapid_z": t("number", "mm/min"), "tool_changer": t("boolean", "automatic tool changer"), "spin_up": json!({"type": "number", "minimum": 0, "description": "seconds to wait after M3 (default 5)"})}), &[], "a machine")
        ], "description": "the machine (preset name or limits)"}),
        ("create_cam" | "edit_cam", "fixtures") => arr(
            obj(json!({"name": t("string", "name"), "min": xyz("box min, work coordinates"), "max": xyz("box max")}), &[], "a clamp or jaw box"),
            "boxes the toolpaths lift over (replaces the list)",
        ),
        ("create_cam" | "edit_cam", "probe") => obj(
            json!({"mode": en(&["off", "plate", "setter"], "probing mode"), "plate_at": xyz("touch plate position, work coordinates"), "plate_thickness": t("number", "mm"), "setter": xyz("tool setter, machine coordinates"), "travel": t("number", "probe distance, mm"), "fast": t("number", "mm/min"), "slow": t("number", "mm/min")}),
            &[],
            "tool measuring after each tool change",
        ),
        ("create_cam" | "edit_cam", "post_text") => obj(
            json!({"header": t("string", "G-code at the start"), "tool_change": t("string", "replaces the standard tool change"), "footer": t("string", "G-code at the end")}),
            &[],
            "your own G-code lines",
        ),
        ("create_cam" | "edit_cam", "follows") => t("string", "name of an earlier Manufacture tab whose leftover stock this setup starts from"),
        ("create_cam" | "edit_cam", "min_rpm") => t("number", "slowest useful spindle speed"),
        ("create_cam" | "edit_cam", "max_rpm") => t("number", "fastest spindle speed"),
        ("create_cam" | "edit_cam", "clearance") => t("number", "rapid height above the stock, mm"),
        ("create_cam" | "edit_cam", "retract") => t("number", "height above the material where feeds start, mm"),
        ("create_cam" | "edit_cam", "rapid_rate") => t("number", "rapid rate, mm/min (for the cycle time)"),
        ("create_cam" | "edit_cam", "arcs") => t("boolean", "write G2/G3 arcs (default true)"),
        ("create_cam" | "edit_cam", "program") => t("integer", "program number"),
        ("import_tool_library", "path") => t("string", "Fusion 360 .json/.tools library or a tools.json"),
        ("import_tool_library", "cam") => json!({"type": "integer", "minimum": 0, "description": "also add every tool to this Manufacture tab"}),
        ("cam_tool_library" | "cam_tool", "kind") => en(&cam_keys(&crate::cam::ToolKind::ALL, crate::cam::ToolKind::key), "tool kind"),
        ("cam_tool_library", "search") => t("string", "search words, e.g. '6 flat'"),
        ("cam_tool", "preset") => t("string", "library search, e.g. 'Ø6 flat end mill'"),
        ("cam_tool", "id") => t("integer", "edit (or delete) this tool"),
        ("cam_tool", "holder") => {
            json!({"oneOf": [t("string", "a holder preset name"), json!({"type": "array", "items": xy("[diameter, height]"), "description": "outline from the holder's bottom up"})], "description": "tool holder"})
        }
        ("cam_tool", "material") => en(&["carbide", "hss", "cobalt"], "tool material"),
        ("cam_op", "kind") => en(&cam_keys(&crate::cam::OpKind::ALL, crate::cam::OpKind::key), "operation kind (to add one)"),
        ("cam_op", "id") => t("integer", "edit (or delete) this operation"),
        ("cam_op", "tool") => t("integer", "tool id or tool number"),
        ("cam_op", "side") => en(&["outside", "inside", "both"], "contour / trace side"),
        ("cam_op", "sketches") => json!({"type": "array", "items": {"type": ["integer", "string"]}, "description": "trace/vcarve: sketch feature ids or names"}),
        ("cam_op", "at") => {
            json!({"type": ["array", "null"], "items": {"type": "number"}, "minItems": 2, "maxItems": 2, "description": "only the pocket or holes around this [x, y]"})
        }
        ("cam_op", "points") => arr(xy("[x, y]"), "explicit hole positions, work coordinates"),
        ("cam_op", "edges") => arr(xyz("[x, y, z]"), "deburr: points on the edges to do"),
        ("simulate_cam", "resolution") => json!({"type": "integer", "minimum": 50, "maximum": 2000, "description": "cells along the longer stock side (default 500)"}),
        ("simulate_cam" | "render_cam" | "render_simulation" | "solve_simulation", "width") => width(900),
        ("simulate_cam" | "render_cam" | "render_simulation" | "solve_simulation", "height") => height(700),
        ("render_cam", "operation") => t("integer", "only this operation id"),
        ("cam_template", "save") => t("string", "save the operations as a template of this name"),
        ("cam_template", "apply") => t("string", "add this template's operations"),
        ("cam_template", "delete") => t("string", "delete this template"),
        ("cam_template", "operations") => arr(t("integer", "operation id"), "operations to save (default all)"),
        ("cam_template", "cam") => common("cam").unwrap(),
        ("cam_soft_jaws", "enabled") => t("boolean", "false removes the jaws"),
        ("cam_soft_jaws", "machine_jaws") => t("boolean", "also make the Manufacture tab that machines the jaws"),
        ("cam_soft_jaws", "print_jaws") => t("boolean", "put both jaws in a Part Studio for printing"),
        ("cam_soft_jaws", "export_step") => out_path("write the printed jaws as STEP here"),
        ("create_cam" | "edit_cam", "save_machine") => t("boolean", "also keep this setup's machine settings as the user's machine"),
        ("create_cam" | "edit_cam" | "cam_tool" | "cam_op", "coolant") => json!({"oneOf": [
            en(&["auto", "off", "flood", "mist"], "coolant (auto: the setup's, or by stock material)"),
            t("boolean", "true = flood, false = auto")
        ], "description": "coolant"}),
        ("cam_tool", "tapping_head") => t("boolean", "taps: held in a self-reversing tapping head (no G84 or spindle reversal)"),
        ("cam_tool", "reverse_ratio") => json!({"type": "number", "exclusiveMinimum": 0, "description": "the tapping head's reverse ratio (default 1.75)"}),
        ("cam_op", "chip_thinning") => t("boolean", "adaptive: raise the cutting feed for radial chip thinning (at most ×1.5)"),
        ("cam_soft_jaws", "one_piece") => t("boolean", "printed only: one block between the vise jaws, squeezed round the part, instead of two jaws"),
        ("cam_soft_jaws", "mount") => {
            let presets: Vec<&str> = crate::cam::JAW_MOUNTS.iter().map(|m| m.1).collect();
            let fields = serde_fields(&crate::cam::JawMount::default(), &[], "mount field");
            json!({"oneOf": [
                json!({"type": "string", "description": format!("a preset ({}) or \"none\"", presets.join(", "))}),
                json!({"type": "null", "description": "no mounting (plain blanks)"}),
                obj(Value::Object(fields), &[], "mounting fields over the current one"),
            ], "description": "how the jaws bolt into the vise"})
        }
        ("insert_standard_part", "standard") => {
            let keys: Vec<&str> = crate::stdparts::STANDARDS.iter().map(|s| s.key).collect();
            json!({"type": "string", "description": format!("standard key or name from list_standard_parts ({})", keys.join(", "))})
        }
        ("insert_standard_part", "size") => {
            json!({"type": ["string", "number"], "description": "size label (\"M5\", \"608\", \"NEMA 17\", \"2020\") or number (default the standard's usual size)"})
        }
        ("insert_standard_part", "length") => json!({"type": "number", "exclusiveMinimum": 0, "description": "length, mm, for screws, pins, shafts, motors and extrusions"}),
        ("export_setup_sheet", "path") => out_path("the .html setup sheet"),
        ("export_gcode", "path") => out_path("the G-code file"),
        ("create_simulation" | "edit_simulation", "material") => en(&fea_materials(), "material for every part (\"\" = each part's own)"),
        ("create_simulation" | "edit_simulation", "mesh_size") => json!({"type": ["number", "null"], "exclusiveMinimum": 0, "description": "element size, mm (null = auto)"}),
        ("create_simulation" | "edit_simulation", "mode") => en(&["simple", "advanced"], "simple: supports and loads; advanced: crackh3d's BCs and solver (static)"),
        ("create_simulation" | "edit_simulation", "solver") => en(&["iterative", "direct"], "advanced: CG+AMG (default) or sparse Cholesky"),
        ("create_simulation" | "edit_simulation", "gravity") => json!({"oneOf": [xyz("acceleration in g"), json!({"type": "null"})], "description": "advanced: acceleration of the whole part in g (null = none)"}),
        ("create_simulation" | "edit_simulation", "bcs") => arr(
            json!({"type": "object", "properties": {
                "type": en(&crate::fea_adv::BcType::KINDS, "what the BC does"),
                "u": json!({"type": "array", "items": {"type": ["number", "null"]}, "minItems": 3, "maxItems": 3, "description": "displacement / remote_rigid: mm per axis, null = free"}),
                "rot": json!({"type": "array", "items": {"type": ["number", "null"]}, "minItems": 3, "maxItems": 3, "description": "remote_rigid: rad per axis, null = free"}),
                "p": t("number", "pressure, MPa"), "t": xyz("traction, MPa"), "f": xyz("force, N"), "k": xyz("spring stiffness per area, N/mm³"),
                "point": xyz("remote reference point"), "force": xyz("remote force, N"), "moment": xyz("remote moment, N·mm"), "at": xyz("point_force: where"),
                "faces": arr(face_pick("a face ref"), "faces it acts on"), "name": t("string", "name")
            }, "required": ["type"], "additionalProperties": false, "description": "one boundary condition"}),
            "advanced boundary conditions (replace the list)",
        ),
        ("create_simulation" | "edit_simulation", "periodic") => arr(
            obj(
                json!({"a": arr(face_pick("a face ref"), "side A (leads)"), "b": arr(face_pick("a face ref"), "side B (follows A)"), "offset": xyz("from A to B (default from the faces' centres)"), "jump": xyz("u_B - u_A, mm"), "name": t("string", "name")}),
                &[],
                "one periodic pair",
            ),
            "advanced periodic pairs (replace the list)",
        ),
        ("create_simulation" | "edit_simulation", "refine") => t("boolean", "refine the mesh at small holes and fillets (default true)"),
        ("create_simulation" | "edit_simulation", "mesh_controls") => arr(
            obj(
                json!({"faces": arr(face_pick("a face ref"), "faces to mesh finer"), "size": json!({"type": "number", "exclusiveMinimum": 0, "description": "element size on the faces, mm"}), "name": t("string", "name")}),
                &["size"],
                "one mesh control",
            ),
            "mesh controls: smaller elements on chosen faces (replace the list)",
        ),
        ("create_simulation" | "edit_simulation", "modes") => json!({"type": "integer", "minimum": 1, "maximum": 50, "description": "modal: how many modes (default 6)"}),
        ("create_simulation" | "edit_simulation", "loss_factors") => {
            json!({"type": "object", "additionalProperties": {"type": "number"}, "description": "structural loss factor η per material name"})
        }
        ("create_simulation" | "edit_simulation", "transient") => obj(
            json!({"profile": en(&["step", "ramp", "pulse", "sine"], "load over time"), "time": t("number", "ramp/pulse length, s"), "frequency": t("number", "sine, Hz"), "duration": json!({"type": ["number", "null"], "description": "s (null = five periods of the lowest mode)"}), "steps": json!({"type": "integer", "minimum": 10, "maximum": 20000, "description": "time steps (default 400)"}), "damping": json!({"type": "number", "minimum": 0, "maximum": 1, "description": "fraction of critical (default 0.02)"})}),
            &[],
            "transient settings",
        ),
        ("create_simulation" | "edit_simulation", "supports") => arr(
            obj(
                json!({"kind": en(&["fixed", "roller"], "support kind (default fixed)"), "faces": arr(face_pick("a face ref"), "faces held"), "name": t("string", "name")}),
                &[],
                "one support",
            ),
            "supports (replace the list)",
        ),
        ("create_simulation" | "edit_simulation", "loads") => arr(
            obj(
                json!({"kind": en(&["force", "pressure", "gravity"], "load kind (default force)"), "value": t("number", "N (force), MPa (pressure) or g (gravity)"),
                    "direction": json!({"oneOf": [en(&["+x", "x", "-x", "+y", "y", "-y", "+z", "z", "-z"], "an axis"), xyz("a vector")], "description": "direction (default -z)"}),
                    "normal": t("boolean", "force pushes into the faces"), "faces": arr(face_pick("a face ref"), "loaded faces (force, pressure)"),
                    "at": xyz("only the faces within radius of this point"), "radius": json!({"type": "number", "exclusiveMinimum": 0, "description": "mm, with at"}), "name": t("string", "name")}),
                &[],
                "one load",
            ),
            "loads (replace the list)",
        ),
        ("mass_properties", "density") => json!({"type": "number", "exclusiveMinimum": 0, "description": "density, g/cm³, for every part (default each part's material)"}),
        ("mass_properties", "material") => en(&materials(), "material for every part (default each part's own)"),
        ("ray_cast", "origins") => arr(xyz("[x, y, z]"), "ray start points, mm"),
        ("ray_cast", "direction") => xyz("ray direction"),
        ("classify_points", "points") => arr(xyz("[x, y, z]"), "points to classify, mm"),
        ("classify_points", "tolerance") => json!({"type": "number", "exclusiveMinimum": 0, "description": "distance counted as on the surface, mm (default 1e-4)"}),
        _ => return None,
    })
}

/// Fill in every tool's schema: shared and specific property specs, the fields CAM tools overlay
/// onto their structs, `rollback` on the modelling tools, and no properties beyond the listed ones.
pub(super) fn complete(tools: &mut [Value]) {
    for tool in tools.iter_mut() {
        let name = tool["name"].as_str().unwrap_or("").to_string();
        let schema = &mut tool["inputSchema"];
        let mut props = schema["properties"].as_object().cloned().unwrap_or_default();
        // fields the CAM tools copy onto their serde structs
        let extra = match name.as_str() {
            "create_cam" | "edit_cam" => serde_fields(&crate::cam::Cam::default(), &["tools", "ops", "next_id", "soft_jaws", "jaws_of"], "setup field"),
            "cam_tool" => serde_fields(&crate::cam::Tool::default(), &["id", "kind", "holder_name"], "tool field"),
            "cam_op" => serde_fields(&crate::cam::Operation::default(), &["id", "kind", "tool"], "operation field"),
            "cam_soft_jaws" => serde_fields(&crate::cam::SoftJaws::default(), &[], "soft jaw field"),
            _ => Map::new(),
        };
        let mut extra = extra;
        // fields serde leaves out at their defaults, so serde_fields can't see them
        let hidden: &[(&str, &str)] = match name.as_str() {
            "cam_tool" => &[("tapping_head", "boolean"), ("reverse_ratio", "number")],
            "cam_op" => &[("chip_thinning", "boolean")],
            _ => &[],
        };
        for (k, ty) in hidden {
            extra.entry(k.to_string()).or_insert(json!({"type": ty, "description": format!("field `{k}`")}));
        }
        if MODELLING_TOOLS.contains(&name.as_str()) {
            props.insert("rollback".into(), common("rollback").unwrap());
        }
        for (k, v) in &extra {
            if !props.contains_key(k) {
                props.insert(k.clone(), v.clone());
            }
        }
        for (k, v) in props.iter_mut() {
            // a struct field keeps its own meaning over a shared name (soft jaw `height` isn't an image's)
            let spec = specific(&name, k).or_else(|| {
                if v.get("description").is_some() || extra.get(k).is_some_and(|x| !matches!(x["type"].as_str(), Some("array" | "object"))) {
                    None
                } else {
                    common(k)
                }
            });
            if let Some(mut s) = spec {
                // a description written next to the tool wins over the catalogue's
                if let (Some(d), Some(o)) = (v.get("description").cloned(), s.as_object_mut()) {
                    o.insert("description".into(), d);
                }
                *v = s;
            }
        }
        for (k, v) in &extra {
            // listed by the tool without a description: the field's one
            if let Some(p) = props.get_mut(k).filter(|p| p.get("description").is_none()) {
                p["description"] = v["description"].clone();
            }
        }
        schema["properties"] = Value::Object(props);
        schema["additionalProperties"] = json!(false);
    }
}

// ---------------------------------------------------------------- checking arguments

/// "; did you mean 'x'?" when one of `options` is close to `word`.
pub(crate) fn did_you_mean<'a>(word: &str, options: impl IntoIterator<Item = &'a str>) -> String {
    let w = word.to_lowercase();
    let best = options.into_iter().map(|o| (edit_distance(&w, &o.to_lowercase()), o)).min_by_key(|x| x.0);
    match best {
        Some((d, o)) if d <= (word.chars().count() / 3).max(2) => format!("; did you mean '{o}'?"),
        _ => String::new(),
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut prev = row[0];
        row[0] = i;
        for j in 1..=b.len() {
            let cur = row[j];
            row[j] = (prev + usize::from(a[i - 1] != b[j - 1])).min(row[j] + 1).min(row[j - 1] + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(n) if n.is_f64() => "a number",
        Value::Number(_) => "an integer",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn type_ok(ty: &str, v: &Value) -> bool {
    match ty {
        "string" => v.is_string(),
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        "null" => v.is_null(),
        _ => true,
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 60 {
        format!("{}…", s.chars().take(60).collect::<String>())
    } else {
        s
    }
}

/// Check `v` against schema `s`; `at` names the value in the error (e.g. `loads[0].faces`).
pub(super) fn validate(s: &Value, v: &Value, at: &str) -> R<()> {
    let name = if at.is_empty() { "arguments" } else { at };
    if let Some(alts) = s.get("oneOf").or_else(|| s.get("anyOf")).and_then(|a| a.as_array()) {
        let mut errs = vec![];
        for alt in alts {
            match validate(alt, v, at) {
                Ok(()) => return Ok(()),
                Err(e) => errs.push((alt.get("type").and_then(|t| t.as_str()).map(|t| type_ok(t, v)).unwrap_or(false), e)),
            }
        }
        // the alternative of the right type says what is wrong; otherwise list what would do
        if let Some((_, e)) = errs.iter().find(|e| e.0) {
            return Err(e.clone());
        }
        let wanted: Vec<String> = alts.iter().filter_map(|a| a.get("description").and_then(|d| d.as_str()).map(String::from)).collect();
        return Err(format!("{name}: got {}, expected {}", short(v), wanted.join(" or ")));
    }
    if let Some(ty) = s.get("type") {
        let types: Vec<&str> = match ty {
            Value::String(t) => vec![t.as_str()],
            Value::Array(a) => a.iter().filter_map(|t| t.as_str()).collect(),
            _ => vec![],
        };
        if !types.is_empty() && !types.iter().any(|t| type_ok(t, v)) {
            return Err(format!("{name}: expected {}, got {} ({})", types.join(" or "), kind_of(v), short(v)));
        }
    }
    if let Some(options) = s.get("enum").and_then(|e| e.as_array()) {
        if !options.contains(v) {
            let list: Vec<String> = options.iter().map(|o| o.as_str().map(String::from).unwrap_or_else(|| o.to_string())).collect();
            let hint = v.as_str().map(|w| did_you_mean(w, list.iter().map(|x| x.as_str()))).unwrap_or_default();
            return Err(format!("{name}: {} is not one of {}{hint}", short(v), list.join(", ")));
        }
    }
    if let Some(x) = v.as_f64() {
        if let Some(m) = s.get("minimum").and_then(|m| m.as_f64()).filter(|m| x < *m) {
            return Err(format!("{name}: {x} is below the minimum {m}"));
        }
        if let Some(m) = s.get("exclusiveMinimum").and_then(|m| m.as_f64()).filter(|m| x <= *m) {
            return Err(format!("{name}: must be more than {m} (got {x})"));
        }
        if let Some(m) = s.get("maximum").and_then(|m| m.as_f64()).filter(|m| x > *m) {
            return Err(format!("{name}: {x} is above the maximum {m}"));
        }
    }
    match v {
        Value::Array(a) => {
            if let Some(n) = s.get("minItems").and_then(|n| n.as_u64()).filter(|n| (a.len() as u64) < *n) {
                return Err(format!("{name}: needs at least {n} items, got {}", a.len()));
            }
            if let Some(n) = s.get("maxItems").and_then(|n| n.as_u64()).filter(|n| (a.len() as u64) > *n) {
                return Err(format!("{name}: takes at most {n} items, got {}", a.len()));
            }
            if let Some(items) = s.get("items") {
                for (i, x) in a.iter().enumerate() {
                    validate(items, x, &format!("{name}[{i}]"))?;
                }
            }
        }
        Value::Object(o) => {
            let props = s.get("properties").and_then(|p| p.as_object());
            for r in s.get("required").and_then(|r| r.as_array()).into_iter().flatten().filter_map(|r| r.as_str()) {
                if !o.contains_key(r) {
                    return Err(if at.is_empty() { format!("give {r}") } else { format!("{name}: give {r}") });
                }
            }
            for (k, x) in o {
                let key = if at.is_empty() { k.clone() } else { format!("{at}.{k}") };
                match (props.and_then(|p| p.get(k)), s.get("additionalProperties")) {
                    (Some(p), _) => validate(p, x, &key)?,
                    (None, Some(ap @ Value::Object(_))) => validate(ap, x, &key)?,
                    (None, Some(Value::Bool(false))) => {
                        let hint = did_you_mean(k, props.into_iter().flat_map(|p| p.keys().map(|s| s.as_str())));
                        let what = if at.is_empty() { "unknown argument".to_string() } else { format!("{name}: unknown key") };
                        return Err(format!("{what} '{k}'{hint}"));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every property of every tool says what it is: a description, a type (or alternatives),
    /// items for arrays and properties for objects.
    #[test]
    fn schemas_are_complete() {
        fn check(tool: &str, path: &str, p: &Value, described: bool, problems: &mut Vec<String>) {
            if described && p.get("description").is_none() {
                problems.push(format!("{tool}.{path}: no description"));
            }
            if let Some(alts) = p.get("oneOf").and_then(|a| a.as_array()) {
                for (i, a) in alts.iter().enumerate() {
                    check(tool, &format!("{path}|{i}"), a, false, problems);
                }
                return;
            }
            let ty = p.get("type");
            if ty.is_none() {
                problems.push(format!("{tool}.{path}: no type"));
            }
            let is = |t: &str| ty.map(|x| x == t || x.as_array().map(|a| a.iter().any(|y| y == t)).unwrap_or(false)).unwrap_or(false);
            if is("array") {
                match p.get("items") {
                    Some(i) => check(tool, &format!("{path}[]"), i, false, problems),
                    None => problems.push(format!("{tool}.{path}: array without items")),
                }
            }
            if is("object") && p.get("properties").is_none() && !p.get("additionalProperties").map(|a| a.is_object() || a == true).unwrap_or(false) {
                problems.push(format!("{tool}.{path}: object without properties"));
            }
            for (k, x) in p.get("properties").and_then(|x| x.as_object()).into_iter().flatten() {
                check(tool, &format!("{path}.{k}"), x, true, problems);
            }
        }
        let mut problems = vec![];
        let tools = crate::agent::tools();
        for t in tools.as_array().unwrap() {
            let name = t["name"].as_str().unwrap();
            assert!(t["description"].as_str().map(|d| !d.is_empty()).unwrap_or(false), "{name} has no description");
            for (k, p) in t["inputSchema"]["properties"].as_object().unwrap() {
                check(name, k, p, true, &mut problems);
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn unknown_keys_and_enums() {
        let tools = crate::agent::tools();
        let schema = |n: &str| tools.as_array().unwrap().iter().find(|t| t["name"] == n).unwrap()["inputSchema"].clone();
        let e = validate(&schema("render"), &json!({"view": "sideways"}), "").unwrap_err();
        assert!(e.contains("view") && e.contains("iso_front_left"), "{e}");
        let e = validate(&schema("render"), &json!({"veiw": "top"}), "").unwrap_err();
        assert!(e.contains("did you mean 'view'"), "{e}");
        assert!(validate(&schema("render"), &json!({"width": -5}), "").is_err());
        assert!(validate(&schema("render"), &json!({"view": "iso_back_left", "width": 400, "direction": [1, 0, 0]}), "").is_ok());
        let e = validate(&schema("list_geometry"), &json!({"type": "vertices"}), "").unwrap_err();
        assert!(e.contains("faces"), "{e}");
        assert!(validate(&schema("list_geometry"), &json!({"normal": "up"}), "").is_err());
        let e = validate(&schema("sketch_add"), &json!({"sketch_id": 1, "items": [{"type": "circle", "center": [0, 0], "raduis": 3}]}), "").unwrap_err();
        assert!(e.contains("items[0]") && e.contains("radius"), "{e}");
        assert!(validate(&schema("create_simulation"), &json!({"loads": [{"kind": "force", "faces": [{"point": [0, 0, 0], "normal": [1, 0, 0]}], "direction": "-y"}]}), "").is_ok());
        assert!(validate(&schema("create_simulation"), &json!({"loads": [{"kind": "force", "direction": "down"}]}), "").is_err());
        assert!(validate(&schema("cam_op"), &json!({"cam": 0, "kind": "pocket", "stepdown": 1.5, "tabs": 3}), "").is_ok());
        assert!(validate(&schema("cam_op"), &json!({"cam": 0, "kind": "teleport"}), "").is_err());
        assert!(validate(&schema("create_sketch"), &json!({"plane": "diagonal"}), "").is_err());
        assert!(validate(&schema("create_sketch"), &json!({"plane": {"kind": "face", "point": [0, 0, 5], "normal": [0, 0, 1]}}), "").is_ok());
    }

    #[test]
    fn suggestions() {
        assert_eq!(did_you_mean("dept", ["depth", "draft", "end"]), "; did you mean 'depth'?");
        assert_eq!(did_you_mean("zzzzzz", ["depth", "draft"]), "");
        assert_eq!(edit_distance("lenght", "length"), 2);
    }
}
