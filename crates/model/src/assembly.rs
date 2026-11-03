//! Assemblies: components, occurrences, in-context references, mates,
//! interference, BOM and assembly export.
//!
//! The document's root fields (`DocData::{params, sketches, features, imports}`) *are*
//! the root component. `DocData::components` is the table of the other component
//! definitions and `DocData::occurrences` the root's children. Each component
//! regenerates in its own local frame with its own regeneration cache; an occurrence
//! places a component in its parent's frame with a rigid [`Transform`].

use std::collections::{BTreeMap, BTreeSet};

use glam::{DAffine3, DMat3, DQuat, DVec3};
use occt::Shape;
use serde::{Deserialize, Serialize};

use crate::document::{ImportEntry, SketchEntry};
use crate::feature::{Feature, PartRef};
use crate::params::Params;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ComponentId(pub u32);

impl ComponentId {
    /// The document itself (its root fields).
    pub const ROOT: ComponentId = ComponentId(0);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OccurrenceId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MateId(pub u32);

/// Path of occurrences from the root (`[]` = the root component itself).
pub type OccPath = Vec<OccurrenceId>;

/// Rigid placement: `p' = rotation * p + translation`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    pub translation: DVec3,
    /// Unit quaternion, serialised as `[x, y, z, w]`.
    pub rotation: DQuat,
}

impl Default for Transform {
    fn default() -> Self {
        Transform::IDENTITY
    }
}

impl Transform {
    pub const IDENTITY: Transform = Transform { translation: DVec3::ZERO, rotation: DQuat::IDENTITY };
    pub fn translation(t: DVec3) -> Self {
        Transform { translation: t, rotation: DQuat::IDENTITY }
    }
    pub fn new(rotation: DQuat, translation: DVec3) -> Self {
        Transform { translation, rotation: rotation.normalize() }
    }
    pub fn affine(&self) -> DAffine3 {
        DAffine3::from_rotation_translation(self.rotation, self.translation)
    }
    pub fn from_affine(a: &DAffine3) -> Self {
        let (_, r, t) = a.to_scale_rotation_translation();
        Transform::new(r, t)
    }
    pub fn mul(&self, o: &Transform) -> Transform {
        Transform::new(self.rotation * o.rotation, self.rotation * o.translation + self.translation)
    }
    pub fn inverse(&self) -> Transform {
        let r = self.rotation.inverse();
        Transform::new(r, -(r * self.translation))
    }
    pub fn point(&self, p: DVec3) -> DVec3 {
        self.rotation * p + self.translation
    }
    pub fn vector(&self, v: DVec3) -> DVec3 {
        self.rotation * v
    }
    pub fn is_identity(&self) -> bool {
        self.translation.length() < 1e-12 && self.rotation.angle_between(DQuat::IDENTITY).abs() < 1e-12
    }
    /// Row-major 3×4 matrix `[r00 r01 r02 tx, r10 .. ty, r20 .. tz]`.
    pub fn to_rows(&self) -> [f64; 12] {
        let m = DMat3::from_quat(self.rotation);
        let t = self.translation;
        [m.x_axis.x, m.y_axis.x, m.z_axis.x, t.x, m.x_axis.y, m.y_axis.y, m.z_axis.y, t.y, m.x_axis.z, m.y_axis.z, m.z_axis.z, t.z]
    }
    pub fn from_rows(r: &[f64]) -> Self {
        let m = DMat3::from_cols(DVec3::new(r[0], r[4], r[8]), DVec3::new(r[1], r[5], r[9]), DVec3::new(r[2], r[6], r[10]));
        Transform::new(DQuat::from_mat3(&m), DVec3::new(r[3], r[7], r[11]))
    }
    /// Apply to a kernel shape (a located copy sharing topology).
    pub fn apply(&self, s: &Shape) -> occt::Result<Shape> {
        let (axis, angle) = self.rotation.to_axis_angle();
        let mut out = if angle.abs() > 1e-12 { s.rotated(DVec3::ZERO, axis, angle.to_degrees())? } else { s.clone() };
        if self.translation.length() > 1e-12 {
            out = out.translated(self.translation)?;
        }
        Ok(out)
    }
}

/// One placement of a component inside its parent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Occurrence {
    pub id: OccurrenceId,
    pub component: ComponentId,
    pub name: String,
    #[serde(default)]
    pub transform: Transform,
    #[serde(default = "yes")]
    pub visible: bool,
    /// Fixed for the mate solver.
    #[serde(default)]
    pub grounded: bool,
    /// Display-only explode offset (parent frame); ignored by mates, interference,
    /// BOM and export.
    #[serde(default)]
    pub explode: DVec3,
    /// Counted in the BOM (false for a design master that other components were
    /// built from in context).
    #[serde(default = "yes")]
    pub in_bom: bool,
}

fn yes() -> bool {
    true
}

/// Where a component's content comes from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum ComponentSource {
    /// Sketches and features stored in this document.
    #[default]
    Native,
    /// Linked external `.cwk` folder: `path` is relative to the host document's
    /// folder; `hash` is the sha256 of its `doc.json` when the content was last
    /// copied in. The embedded content is a cache so the host opens without the file.
    Linked { path: String, hash: String },
    /// A node of an imported STEP assembly (blob sha256, node index in the XCAF tree).
    /// Its own geometry is one locked reference body; its children are occurrences.
    Step { blob: String, node: u32 },
}

/// A component definition.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Component {
    pub id: ComponentId,
    pub name: String,
    /// Local parameters; they shadow document parameters of the same name.
    #[serde(default)]
    pub params: Params,
    #[serde(default)]
    pub sketches: Vec<SketchEntry>,
    #[serde(default)]
    pub features: Vec<Feature>,
    #[serde(default)]
    pub imports: Vec<ImportEntry>,
    #[serde(default)]
    pub children: Vec<Occurrence>,
    #[serde(default)]
    pub source: ComponentSource,
    /// Locked components (STEP references, linked files) cannot be edited.
    #[serde(default)]
    pub locked: bool,
    /// Bought rather than made (library parts, PCBs). Its BOM line uses `part`.
    #[serde(default)]
    pub purchased: Option<PartRef>,
    /// Process profile name used to make it (`None` = the document's active one).
    #[serde(default)]
    pub process: Option<String>,
}

/// How the two connectors of a mate move relative to each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MateKind {
    /// No relative motion.
    Rigid,
    /// Rotation about the common z axis (`angle`).
    Revolute,
    /// Translation along the common z axis (`distance`).
    Slider,
    /// z axes coincide; rotation about and translation along it are free.
    Cylindrical,
    /// The xy planes coincide (z opposite after `flip`); free in-plane motion.
    Planar,
    /// Origins coincide; rotation free.
    Ball,
}

impl MateKind {
    /// Remaining degrees of freedom.
    pub fn dof(self) -> u32 {
        match self {
            MateKind::Rigid => 0,
            MateKind::Revolute | MateKind::Slider => 1,
            MateKind::Cylindrical => 2,
            MateKind::Planar | MateKind::Ball => 3,
        }
    }
}

/// A frame on an occurrence's geometry, in that occurrence's component frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MateConnector {
    pub occurrence: OccPath,
    pub origin: DVec3,
    /// Connector z (face normal, circle axis).
    pub z: DVec3,
    /// Connector x (any direction ⟂ z).
    pub x: DVec3,
}

fn perp(z: DVec3) -> DVec3 {
    let a = if z.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
    (a - z * a.dot(z)).normalize()
}

impl MateConnector {
    pub fn new(occurrence: OccPath, origin: DVec3, z: DVec3) -> Self {
        let z = z.normalize();
        MateConnector { occurrence, origin, z, x: perp(z) }
    }
    /// Snap to a planar face: its centroid, z = outward normal.
    pub fn from_face(occurrence: OccPath, f: &occt::FaceInfo) -> Self {
        Self::new(occurrence, f.centroid, f.normal)
    }
    /// Snap to a circular edge: its centre, z = its axis. `None` if not circular.
    pub fn from_circular_edge(occurrence: OccPath, e: &occt::EdgeInfo) -> Option<Self> {
        e.circle.map(|(c, axis, _)| Self::new(occurrence, c, axis))
    }
    /// Snap to a point (z = +Z of the component).
    pub fn point(occurrence: OccPath, p: DVec3) -> Self {
        Self::new(occurrence, p, DVec3::Z)
    }
    /// Local frame as a transform (columns x, y, z at origin).
    pub fn frame(&self) -> Transform {
        let z = self.z.normalize();
        let x = (self.x - z * self.x.dot(z)).try_normalize().unwrap_or_else(|| perp(z));
        let y = z.cross(x);
        Transform::new(DQuat::from_mat3(&DMat3::from_cols(x, y, z)), self.origin)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mate {
    pub id: MateId,
    pub name: String,
    pub kind: MateKind,
    /// The connector that stays (when its occurrence is placed first).
    pub a: MateConnector,
    /// The connector that is moved onto `a`.
    pub b: MateConnector,
    /// Turn `b` over (z reversed): faces that touch face each other.
    #[serde(default)]
    pub flip: bool,
    /// Revolute drive value (degrees about z). Cylindrical, Planar and Ball keep
    /// their current position in their free directions.
    #[serde(default = "zero")]
    pub angle: crate::params::Expr,
    /// Slider drive value (mm along z), also the z gap of Rigid, Revolute and Planar.
    #[serde(default = "zero")]
    pub distance: crate::params::Expr,
    #[serde(default)]
    pub suppressed: bool,
}

fn zero() -> crate::params::Expr {
    crate::params::Expr::new("0")
}

/// Every occurrence path in the tree below `parent`, depth first.
pub(crate) fn walk<'a>(
    comps: &'a BTreeMap<ComponentId, &'a Component>,
    children: &'a [Occurrence],
    prefix: &OccPath,
    world: Transform,
    out: &mut Vec<(OccPath, &'a Occurrence, Transform)>,
    depth: usize,
) {
    if depth > 64 {
        return;
    }
    for o in children {
        let mut p = prefix.clone();
        p.push(o.id);
        let w = world.mul(&o.transform);
        out.push((p.clone(), o, w));
        if let Some(c) = comps.get(&o.component) {
            walk(comps, &c.children, &p, w, out, depth + 1);
        }
    }
}



// ---------------------------------------------------------------- tree access

use crate::document::{DocData, DocError, Document};
use crate::feature::{FeatureId, FeatureKind, SketchId};
use crate::sketch::Sketch;

impl DocData {
    pub fn component(&self, id: ComponentId) -> Option<&Component> {
        self.components.iter().find(|c| c.id == id)
    }
    pub fn component_mut(&mut self, id: ComponentId) -> Option<&mut Component> {
        self.components.iter_mut().find(|c| c.id == id)
    }
    /// Children of a component (`ROOT` = the document's top-level occurrences).
    pub fn children_of(&self, id: ComponentId) -> Option<&[Occurrence]> {
        if id == ComponentId::ROOT {
            Some(&self.occurrences)
        } else {
            self.component(id).map(|c| c.children.as_slice())
        }
    }
    fn children_mut(&mut self, id: ComponentId) -> Option<&mut Vec<Occurrence>> {
        if id == ComponentId::ROOT {
            Some(&mut self.occurrences)
        } else {
            self.component_mut(id).map(|c| &mut c.children)
        }
    }
    /// An occurrence anywhere in the tree (ids are unique per document).
    pub fn occurrence(&self, id: OccurrenceId) -> Option<&Occurrence> {
        self.occurrences.iter().chain(self.components.iter().flat_map(|c| c.children.iter())).find(|o| o.id == id)
    }
    pub fn occurrence_mut(&mut self, id: OccurrenceId) -> Option<&mut Occurrence> {
        if let Some(o) = self.occurrences.iter_mut().find(|o| o.id == id) {
            return Some(o);
        }
        self.components.iter_mut().flat_map(|c| c.children.iter_mut()).find(|o| o.id == id)
    }
    /// The component an occurrence path ends in (`[]` = root).
    pub fn component_at(&self, path: &[OccurrenceId]) -> Result<ComponentId, String> {
        let mut cur = ComponentId::ROOT;
        for id in path {
            let kids = self.children_of(cur).ok_or_else(|| format!("component {cur:?} does not exist"))?;
            cur = kids.iter().find(|o| o.id == *id).ok_or_else(|| format!("occurrence {id:?} is not a child of {}", self.component_name(cur)))?.component;
        }
        Ok(cur)
    }
    /// World (root-frame) transform of an occurrence path.
    pub fn world_transform(&self, path: &[OccurrenceId]) -> Result<Transform, String> {
        let mut cur = ComponentId::ROOT;
        let mut t = Transform::IDENTITY;
        for id in path {
            let kids = self.children_of(cur).ok_or_else(|| format!("component {cur:?} does not exist"))?;
            let o = kids.iter().find(|o| o.id == *id).ok_or_else(|| format!("occurrence {id:?} is not a child of {}", self.component_name(cur)))?;
            t = t.mul(&o.transform);
            cur = o.component;
        }
        Ok(t)
    }
    pub fn component_name(&self, id: ComponentId) -> String {
        if id == ComponentId::ROOT {
            self.name.clone()
        } else {
            self.component(id).map(|c| c.name.clone()).unwrap_or_else(|| format!("component {}", id.0))
        }
    }
    /// True if `inner` is `outer` or occurs (transitively) inside it.
    pub fn contains_component(&self, outer: ComponentId, inner: ComponentId) -> bool {
        fn rec(d: &DocData, c: ComponentId, inner: ComponentId, depth: usize) -> bool {
            if c == inner {
                return true;
            }
            depth < 64 && d.children_of(c).is_some_and(|k| k.iter().any(|o| rec(d, o.component, inner, depth + 1)))
        }
        rec(self, outer, inner, 0)
    }
    /// Every occurrence path in the tree with its world transform, depth first.
    pub fn all_occurrences(&self) -> Vec<(OccPath, Occurrence, Transform)> {
        let comps: BTreeMap<ComponentId, &Component> = self.components.iter().map(|c| (c.id, c)).collect();
        let mut out = Vec::new();
        walk(&comps, &self.occurrences, &Vec::new(), Transform::IDENTITY, &mut out, 0);
        out.into_iter().map(|(p, o, t)| (p, o.clone(), t)).collect()
    }

    /// The document as seen by one component's regeneration: its sketches, features
    /// and imports, the document params with the component's local params on top,
    /// and its process profile. `None` for an unknown id.
    pub fn component_view(&self, id: ComponentId) -> Option<DocData> {
        let mut d = DocData { components: Vec::new(), occurrences: Vec::new(), mates: Vec::new(), drawings: Vec::new(), ..self.clone() };
        if id == ComponentId::ROOT {
            return Some(d);
        }
        let c = self.component(id)?;
        for p in &c.params.list {
            match d.params.list.iter_mut().find(|q| q.name == p.name) {
                Some(q) => *q = p.clone(),
                None => d.params.list.push(p.clone()),
            }
        }
        d.name = c.name.clone();
        d.sketches = c.sketches.clone();
        d.features = c.features.clone();
        d.imports = c.imports.clone();
        d.body_orientations.clear();
        d.teardrop_overrides.clear();
        if let Some(p) = &c.process {
            d.active_profile = p.clone();
        }
        Some(d)
    }
}

impl Document {
    /// Add an empty component definition (no occurrence yet).
    pub fn add_component(&mut self, name: &str) -> ComponentId {
        self.edit(&format!("New component {name}"), |d| {
            let id = ComponentId(d.alloc());
            d.components.push(Component { id, name: name.into(), ..Default::default() });
            Ok::<_, DocError>(id)
        })
        .unwrap()
    }

    /// Place `component` inside `parent` (`ComponentId::ROOT` for the top level).
    /// Fails if that would nest a component inside itself.
    pub fn add_occurrence(&mut self, parent: ComponentId, component: ComponentId, name: &str, transform: Transform) -> Result<OccurrenceId, DocError> {
        self.edit(&format!("Insert {name}"), |d| {
            if component == ComponentId::ROOT || d.component(component).is_none() {
                return Err(DocError::Other(format!("component {component:?} does not exist")));
            }
            if d.contains_component(component, parent) {
                return Err(DocError::Other(format!(
                    "component nesting cycle: `{}` would contain itself",
                    d.component_name(component)
                )));
            }
            let id = OccurrenceId(d.alloc());
            let o = Occurrence { id, component, name: name.into(), transform, visible: true, grounded: false, explode: DVec3::ZERO, in_bom: true };
            d.children_mut(parent).ok_or_else(|| DocError::Other(format!("component {parent:?} does not exist")))?.push(o);
            Ok(id)
        })
    }

    /// Undoable change to one occurrence (transform, visibility, grounded, explode).
    pub fn edit_occurrence<T>(&mut self, id: OccurrenceId, label: &str, f: impl FnOnce(&mut Occurrence) -> T) -> Result<T, DocError> {
        self.edit(label, |d| d.occurrence_mut(id).map(f).ok_or_else(|| DocError::Other(format!("occurrence {id:?} does not exist"))))
    }

    /// Remove an occurrence (and mates that reference it).
    pub fn remove_occurrence(&mut self, id: OccurrenceId) -> Result<(), DocError> {
        self.edit("Remove occurrence", |d| {
            let before = d.occurrences.len() + d.components.iter().map(|c| c.children.len()).sum::<usize>();
            d.occurrences.retain(|o| o.id != id);
            for c in &mut d.components {
                c.children.retain(|o| o.id != id);
            }
            let after = d.occurrences.len() + d.components.iter().map(|c| c.children.len()).sum::<usize>();
            if before == after {
                return Err(DocError::Other(format!("occurrence {id:?} does not exist")));
            }
            d.mates.retain(|m| !m.a.occurrence.contains(&id) && !m.b.occurrence.contains(&id));
            Ok(())
        })
    }

    /// Undoable change to a component definition. Locked components (STEP
    /// references, linked files) only accept changes that leave their content
    /// (params, sketches, features, imports) untouched.
    pub fn edit_component<T>(&mut self, id: ComponentId, label: &str, f: impl FnOnce(&mut Component) -> T) -> Result<T, DocError> {
        self.edit(label, |d| {
            let c = d.component_mut(id).ok_or_else(|| DocError::Other(format!("component {id:?} does not exist")))?;
            let before = (c.params.clone(), c.sketches.clone(), c.features.clone(), c.imports.clone());
            let r = f(c);
            if c.locked && before != (c.params.clone(), c.sketches.clone(), c.features.clone(), c.imports.clone()) {
                return Err(DocError::Other(format!("component `{}` is locked (break the link to edit it)", c.name)));
            }
            Ok(r)
        })
    }

    /// Add a sketch to a component (`ROOT` = the document).
    pub fn add_sketch_in(&mut self, comp: ComponentId, sketch: Sketch) -> Result<SketchId, DocError> {
        if comp == ComponentId::ROOT {
            return Ok(self.add_sketch(sketch));
        }
        let id = SketchId(self.data.next_id);
        self.edit_component(comp, &format!("Add sketch {}", sketch.name), |c| c.sketches.push(SketchEntry { id, sketch }))?;
        self.data.next_id += 1;
        Ok(id)
    }

    /// Add a feature to a component (`ROOT` = the document).
    pub fn add_feature_in(&mut self, comp: ComponentId, name: &str, kind: FeatureKind) -> Result<FeatureId, DocError> {
        if comp == ComponentId::ROOT {
            return Ok(self.add_feature(name, kind));
        }
        let id = FeatureId(self.data.next_id);
        let label = format!("{} {name}", kind.label());
        self.edit_component(comp, &label, |c| c.features.push(Feature { id, name: name.into(), suppressed: false, kind }))?;
        self.data.next_id += 1;
        Ok(id)
    }

    /// Set a component-local parameter (shadows a document parameter of that name).
    pub fn set_component_param(&mut self, comp: ComponentId, name: &str, expr: impl Into<crate::params::Expr>) -> Result<(), DocError> {
        if comp == ComponentId::ROOT {
            return self.set_param(name, expr);
        }
        let expr = expr.into();
        let c = self.data.component(comp).ok_or_else(|| DocError::Other(format!("component {comp:?} does not exist")))?;
        let mut local = c.params.clone();
        local.set(name, expr)?;
        let mut all = self.data.params.clone();
        for p in &local.list {
            all.set(&p.name, p.expr.clone())?;
        }
        all.evaluate()?;
        self.edit_component(comp, &format!("Set {name}"), |c| c.params = local)
    }
}

// ---------------------------------------------------------------- regeneration

use std::collections::HashMap;
use std::sync::Arc;

use crate::document::BlobStore;
use crate::library::Library;
use crate::regen::{Body, ContextBody, RegenResult, Regenerator};

/// One component's regeneration.
#[derive(Clone)]
pub struct ComponentResult {
    pub component: ComponentId,
    pub name: String,
    pub regen: RegenResult,
}

/// A body placed in the assembly: one per (occurrence path, body of its component).
#[derive(Clone, Debug)]
pub struct Instance {
    /// Occurrence path (`[]` = the root component's own bodies).
    pub path: OccPath,
    pub component: ComponentId,
    /// The body in its component's frame (shared by every occurrence).
    pub body: Body,
    /// Component frame → world.
    pub transform: Transform,
    /// `transform` with the explode offsets of the path applied (display only).
    pub display: Transform,
    /// Every occurrence on the path is visible.
    pub visible: bool,
    /// Every occurrence on the path is counted in the BOM.
    pub in_bom: bool,
}

impl Instance {
    /// The body's shape in world coordinates.
    pub fn world_shape(&self) -> occt::Result<Shape> {
        self.transform.apply(self.body.shape())
    }
    /// Instance name for reports: "Occ:1 / Sub:2 / Body".
    pub fn label(&self, d: &DocData) -> String {
        let mut parts: Vec<String> = self.path.iter().filter_map(|o| d.occurrence(*o).map(|o| o.name.clone())).collect();
        parts.push(self.body.name.clone());
        parts.join(" / ")
    }
}

/// A problem with the assembly structure (not a feature failure).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AssemblyIssue {
    pub component: Option<ComponentId>,
    pub message: String,
}

#[derive(Clone)]
pub struct AssemblyResult {
    /// Per component (including `ROOT`), in regeneration order.
    pub components: Vec<ComponentResult>,
    pub instances: Vec<Instance>,
    /// Dependency cycles, unknown components.
    pub issues: Vec<AssemblyIssue>,
}

impl AssemblyResult {
    pub fn component(&self, id: ComponentId) -> Option<&ComponentResult> {
        self.components.iter().find(|c| c.component == id)
    }
    /// Instances of one occurrence path (its component's bodies, placed).
    pub fn instances_of(&self, path: &[OccurrenceId]) -> std::vec::IntoIter<&Instance> {
        self.instances.iter().filter(|i| i.path == path).collect::<Vec<_>>().into_iter()
    }
    /// Every feature error of every component, plus structural issues.
    pub fn errors(&self) -> Vec<String> {
        let mut v: Vec<String> = self.issues.iter().map(|i| i.message.clone()).collect();
        for c in &self.components {
            for (f, e) in c.regen.errors() {
                v.push(format!("{} / feature {}: {e}", c.name, f.0));
            }
        }
        v
    }
}

/// Per-component regeneration caches (one `Regenerator` per component, so editing
/// one component never invalidates another's cached states).
#[derive(Default)]
pub struct AssemblyCache {
    regens: HashMap<ComponentId, Regenerator>,
}

impl AssemblyCache {
    pub fn regenerator(&mut self, id: ComponentId) -> &mut Regenerator {
        self.regens.entry(id).or_default()
    }
    pub fn clear(&mut self) {
        self.regens.clear();
    }
}

/// Components `c` references in context (by occurrence path).
fn context_sources(d: &DocData, c: ComponentId) -> Vec<OccPath> {
    let feats: &[Feature] = if c == ComponentId::ROOT { &d.features } else { d.component(c).map(|c| c.features.as_slice()).unwrap_or(&[]) };
    let mut v: Vec<OccPath> = feats
        .iter()
        .filter(|f| !f.suppressed)
        .filter_map(|f| match &f.kind {
            FeatureKind::ContextRef { source, .. } => Some(source.clone()),
            _ => None,
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Order components so in-context sources come first; components on a cycle are
/// reported and appended at the end. Returns (order, cycles as name chains).
fn regen_order(d: &DocData) -> (Vec<ComponentId>, Vec<(Vec<ComponentId>, String)>) {
    let mut ids = vec![ComponentId::ROOT];
    ids.extend(d.components.iter().map(|c| c.id));
    let deps: BTreeMap<ComponentId, Vec<ComponentId>> =
        ids.iter().map(|c| (*c, context_sources(d, *c).iter().filter_map(|p| d.component_at(p).ok()).collect())).collect();
    #[derive(Clone, Copy, PartialEq)]
    enum St {
        Visiting,
        Done,
    }
    fn visit(
        c: ComponentId,
        deps: &BTreeMap<ComponentId, Vec<ComponentId>>,
        st: &mut BTreeMap<ComponentId, St>,
        stack: &mut Vec<ComponentId>,
        order: &mut Vec<ComponentId>,
        cycles: &mut Vec<Vec<ComponentId>>,
    ) {
        match st.get(&c) {
            Some(St::Done) => return,
            Some(St::Visiting) => {
                let i = stack.iter().position(|x| *x == c).unwrap_or(0);
                let mut cyc = stack[i..].to_vec();
                cyc.push(c);
                cycles.push(cyc);
                return;
            }
            None => {}
        }
        st.insert(c, St::Visiting);
        stack.push(c);
        for dep in deps.get(&c).into_iter().flatten() {
            visit(*dep, deps, st, stack, order, cycles);
        }
        stack.pop();
        st.insert(c, St::Done);
        order.push(c);
    }
    let (mut st, mut stack, mut order, mut cycles) = (BTreeMap::new(), Vec::new(), Vec::new(), Vec::new());
    for c in &ids {
        visit(*c, &deps, &mut st, &mut stack, &mut order, &mut cycles);
    }
    let named = cycles
        .into_iter()
        .map(|cyc| {
            let s = cyc.iter().map(|c| d.component_name(*c)).collect::<Vec<_>>().join(" -> ");
            (cyc, format!("in-context dependency cycle: {s}"))
        })
        .collect();
    (order, named)
}

/// First occurrence path of each component (its in-context editing frame).
fn first_paths(d: &DocData) -> HashMap<ComponentId, (OccPath, Transform)> {
    let mut m = HashMap::new();
    m.insert(ComponentId::ROOT, (Vec::new(), Transform::IDENTITY));
    for (p, o, w) in d.all_occurrences() {
        m.entry(o.component).or_insert((p, w));
    }
    m
}

/// Regenerate every component (dependencies first) and place the instances.
/// `root` is the document's own regenerator (shared with `Document::regenerate`).
pub fn regenerate_assembly(d: &DocData, blobs: &BlobStore, lib: &Library, root: &mut Regenerator, cache: &mut AssemblyCache) -> AssemblyResult {
    let (order, cycles) = regen_order(d);
    let mut issues: Vec<AssemblyIssue> = cycles.iter().map(|(c, m)| AssemblyIssue { component: c.first().copied(), message: m.clone() }).collect();
    let on_cycle: BTreeMap<ComponentId, String> = cycles.iter().flat_map(|(c, m)| c.iter().map(move |x| (*x, m.clone()))).collect();
    let frames = first_paths(d);
    let mut results: Vec<ComponentResult> = Vec::new();
    for c in order {
        let Some(view) = d.component_view(c) else { continue };
        // in-context inputs, placed into this component's frame
        let host = frames.get(&c).map(|f| f.1).unwrap_or(Transform::IDENTITY);
        let mut ctx = HashMap::new();
        let mut keys = HashMap::new();
        let mut errs = HashMap::new();
        for src in context_sources(d, c) {
            let sc = match d.component_at(&src) {
                Ok(sc) => sc,
                Err(e) => {
                    errs.insert(src, format!("in-context source: {e}"));
                    continue;
                }
            };
            if let (Some(m), true) = (on_cycle.get(&c), on_cycle.contains_key(&sc)) {
                errs.insert(src, m.clone());
                continue;
            }
            let Some(sr) = results.iter().find(|r| r.component == sc) else {
                errs.insert(src, format!("in-context source `{}` has not been regenerated", d.component_name(sc)));
                continue;
            };
            let rel = match d.world_transform(&src) {
                Ok(w) => host.inverse().mul(&w),
                Err(e) => {
                    errs.insert(src, e);
                    continue;
                }
            };
            let mut bodies = Vec::new();
            let mut h = std::hash::DefaultHasher::new();
            for b in &sr.regen.bodies {
                match rel.apply(b.shape()) {
                    Ok(s) => {
                        use std::hash::Hash;
                        b.name.hash(&mut h);
                        (Arc::as_ptr(&b.geom) as usize).hash(&mut h);
                        bodies.push(ContextBody { name: b.name.clone(), reference: b.reference, shape: s });
                    }
                    Err(e) => {
                        errs.insert(src.clone(), e.to_string());
                    }
                }
            }
            {
                use std::hash::{Hash, Hasher};
                rel.to_rows().iter().for_each(|x| x.to_bits().hash(&mut h));
                keys.insert(src.clone(), h.finish());
            }
            ctx.insert(src, Arc::new(bodies));
        }
        let regen = if c == ComponentId::ROOT { &mut *root } else { cache.regenerator(c) };
        regen.context = ctx;
        regen.context_keys = keys;
        regen.context_errors = errs;
        let r = match d.component(c).map(|c| &c.source) {
            Some(ComponentSource::Step { blob, node }) => step_component(d, c, blob, *node, blobs, cache),
            _ => regen.regenerate(&view, blobs, lib),
        };
        results.push(ComponentResult { component: c, name: d.component_name(c), regen: r });
    }
    for c in &d.components {
        if !results.iter().any(|r| r.component == c.id) {
            issues.push(AssemblyIssue { component: Some(c.id), message: format!("component `{}` was not regenerated", c.name) });
        }
    }
    // instances
    let mut instances = Vec::new();
    if let Some(r) = results.iter().find(|r| r.component == ComponentId::ROOT) {
        for b in &r.regen.bodies {
            instances.push(Instance { path: Vec::new(), component: ComponentId::ROOT, body: b.clone(), transform: Transform::IDENTITY, display: Transform::IDENTITY, visible: true, in_bom: true });
        }
    }
    let comps: BTreeMap<ComponentId, &Component> = d.components.iter().map(|c| (c.id, c)).collect();
    type Frame<'a> = (OccPath, Transform, Transform, bool, bool, &'a [Occurrence]);
    let mut stack: Vec<Frame> = vec![(Vec::new(), Transform::IDENTITY, Transform::IDENTITY, true, true, &d.occurrences)];
    while let Some((prefix, world, disp, vis, bom, kids)) = stack.pop() {
        if prefix.len() > 64 {
            continue;
        }
        for o in kids.iter().rev() {
            let mut p = prefix.clone();
            p.push(o.id);
            let w = world.mul(&o.transform);
            let dt = disp.mul(&Transform::translation(o.explode)).mul(&o.transform);
            let (v, ib) = (vis && o.visible, bom && o.in_bom);
            match results.iter().find(|r| r.component == o.component) {
                Some(r) => {
                    for b in &r.regen.bodies {
                        instances.push(Instance { path: p.clone(), component: o.component, body: b.clone(), transform: w, display: dt, visible: v, in_bom: ib });
                    }
                }
                None => issues.push(AssemblyIssue { component: Some(o.component), message: format!("occurrence `{}` refers to a missing component", o.name) }),
            }
            if let Some(c) = comps.get(&o.component) {
                stack.push((p, w, dt, v, ib, &c.children));
            }
        }
    }
    instances.sort_by(|a, b| a.path.cmp(&b.path));
    AssemblyResult { components: results, instances, issues }
}

/// Placeholder for STEP reference components (filled in by the STEP assembly import).
fn step_component(_d: &DocData, _c: ComponentId, _blob: &str, _node: u32, _blobs: &BlobStore, _cache: &mut AssemblyCache) -> RegenResult {
    RegenResult { bodies: Vec::new(), reports: Vec::new(), recomputed: 0, reused: 0, process: Default::default() }
}

impl Document {
    /// Regenerate the whole tree (cached per component). Cheap when nothing changed.
    pub fn regenerate_assembly(&mut self) -> &AssemblyResult {
        if self.asm_last.is_none() {
            let r = regenerate_assembly(&self.data, &self.blobs, &self.library, &mut self.regen, &mut self.asm);
            self.asm_last = Some(r);
        }
        self.asm_last.as_ref().unwrap()
    }

    /// Regeneration of one component in its local frame.
    pub fn component_result(&mut self, id: ComponentId) -> Option<RegenResult> {
        self.regenerate_assembly().component(id).map(|c| c.regen.clone())
    }
}

impl Document {
    /// A library part as a (locked, purchased) component: one reference body at the
    /// component origin, axis +Z. Place it with `add_occurrence`.
    pub fn add_part_component(&mut self, info: &crate::library::PartInfo, size: &str) -> Result<ComponentId, DocError> {
        let part = self.part_ref(info, size)?;
        let name = format!("{} {}", part.name, part.size);
        self.edit(&format!("New component {name}"), |d| {
            let id = ComponentId(d.alloc());
            let fid = FeatureId(d.alloc());
            let feature = Feature {
                id: fid,
                name: name.clone(),
                suppressed: false,
                kind: FeatureKind::InsertPart {
                    part: part.clone(),
                    placements: vec![crate::feature::PlacementRef::Explicit { origin: DVec3::ZERO, axis: DVec3::Z }],
                    flip: false,
                    offset: crate::params::Expr::new("0"),
                    auto_hole: None,
                    target: None,
                },
            };
            d.components.push(Component { id, name: name.clone(), features: vec![feature], locked: true, purchased: Some(part), ..Default::default() });
            Ok::<_, DocError>(id)
        })
    }
}

// ---------------------------------------------------------------- mates

/// Outcome of one mate in [`Document::solve_mates`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum MateStatus {
    /// Placed its moving occurrence.
    Solved,
    /// Both sides were already placed and the mate holds.
    Redundant,
    /// Both sides were already placed and the mate does not hold (by how much).
    OverConstrained(String),
    /// Not solvable (neither side reaches a grounded occurrence, bad path, ...).
    Unsolved(String),
    Suppressed,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MateReport {
    pub mate: MateId,
    pub name: String,
    pub status: MateStatus,
    /// Degrees of freedom the mate leaves (`MateKind::dof`).
    pub dof: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MateSolve {
    pub mates: Vec<MateReport>,
    /// Top-level occurrences whose transform was changed.
    pub moved: Vec<OccurrenceId>,
    /// Top-level occurrences with mates that are not connected to anything fixed.
    pub floating: Vec<OccurrenceId>,
    /// Notes (e.g. "nothing grounded: `Base:1` was held fixed").
    pub notes: Vec<String>,
}

impl MateSolve {
    pub fn ok(&self) -> bool {
        self.mates.iter().all(|m| matches!(m.status, MateStatus::Solved | MateStatus::Redundant | MateStatus::Suppressed))
    }
}

fn flip_x() -> Transform {
    Transform::new(DQuat::from_rotation_x(std::f64::consts::PI), DVec3::ZERO)
}

/// Rotation angle of `t` about z (from its x axis projected on the xy plane).
fn z_angle(t: &Transform) -> f64 {
    let x = t.vector(DVec3::X);
    x.y.atan2(x.x)
}

/// The allowed relative placement of B's connector in A's (flipped) connector frame
/// closest to `rel` (the current one), given the mate's kind and drive values.
fn project_motion(kind: MateKind, rel: &Transform, angle_deg: f64, dist: f64) -> Transform {
    let rz = |a: f64| DQuat::from_rotation_z(a);
    match kind {
        MateKind::Rigid => Transform::translation(DVec3::Z * dist),
        MateKind::Revolute => Transform::new(rz(angle_deg.to_radians()), DVec3::Z * dist),
        MateKind::Slider => Transform::translation(DVec3::Z * dist),
        MateKind::Cylindrical => Transform::new(rz(z_angle(rel)), DVec3::Z * rel.translation.z),
        MateKind::Planar => Transform::new(rz(z_angle(rel)), DVec3::new(rel.translation.x, rel.translation.y, dist)),
        MateKind::Ball => Transform::new(rel.rotation, DVec3::ZERO),
    }
}

/// Mismatch between two placements: (distance mm, angle deg).
fn mismatch(a: &Transform, b: &Transform) -> (f64, f64) {
    ((a.translation - b.translation).length(), a.rotation.angle_between(b.rotation).to_degrees())
}

impl Document {
    pub fn add_mate(&mut self, name: &str, kind: MateKind, a: MateConnector, b: MateConnector, flip: bool) -> MateId {
        self.edit(&format!("Mate {name}"), |d| {
            let id = MateId(d.alloc());
            d.mates.push(Mate { id, name: name.into(), kind, a, b, flip, angle: zero(), distance: zero(), suppressed: false });
            Ok::<_, DocError>(id)
        })
        .unwrap()
    }

    pub fn edit_mate<T>(&mut self, id: MateId, label: &str, f: impl FnOnce(&mut Mate) -> T) -> Result<T, DocError> {
        self.edit(label, |d| d.mates.iter_mut().find(|m| m.id == id).map(f).ok_or_else(|| DocError::Other(format!("mate {id:?} does not exist"))))
    }

    /// Solve the mates and write the resulting transforms of the top-level
    /// occurrences into the document (one undo step "Solve mates" when anything
    /// moved). The report is returned either way.
    pub fn solve_mates(&mut self) -> MateSolve {
        let (tf, rep) = solve_mates(&self.data);
        if !rep.moved.is_empty() {
            let _ = self.edit("Solve mates", |d| {
                for (id, t) in &tf {
                    if let Some(o) = d.occurrences.iter_mut().find(|o| o.id == *id) {
                        o.transform = *t;
                    }
                }
                Ok::<_, DocError>(())
            });
        }
        rep
    }
}

/// Solve without modifying the document: new transforms of the top-level
/// occurrences that moved, plus the report.
///
/// A simple propagation solver: grounded top-level occurrences (and the root's own
/// bodies) are fixed; a mate with one placed side places the other side's top-level
/// occurrence (a whole sub-assembly moves rigidly), keeping its current position in
/// the mate's free degrees of freedom. A mate whose sides are both placed is checked
/// (redundant or over-constrained). Mates that never get a placed side are unsolved.
pub fn solve_mates(d: &DocData) -> (Vec<(OccurrenceId, Transform)>, MateSolve) {
    let mut rep = MateSolve::default();
    let vals = d.values().unwrap_or_default();
    // current transform of each top-level occurrence; None key = root (fixed)
    let mut cur: BTreeMap<OccurrenceId, Transform> = d.occurrences.iter().map(|o| (o.id, o.transform)).collect();
    let mut placed: BTreeSet<Option<OccurrenceId>> = BTreeSet::new();
    placed.insert(None);
    for o in d.occurrences.iter().filter(|o| o.grounded) {
        placed.insert(Some(o.id));
    }
    let active: Vec<&Mate> = d.mates.iter().filter(|m| !m.suppressed).collect();
    if d.occurrences.iter().all(|o| !o.grounded) {
        let first = active.iter().find_map(|m| m.a.occurrence.first().or(m.b.occurrence.first()).copied());
        if let Some(f) = first {
            placed.insert(Some(f));
            rep.notes.push(format!("nothing is grounded: `{}` was held fixed", d.occurrence(f).map(|o| o.name.as_str()).unwrap_or("?")));
        }
    }
    // connector frame relative to its top-level occurrence: (top, rest * frame)
    let local = |c: &MateConnector| -> Result<(Option<OccurrenceId>, Transform), String> {
        d.component_at(&c.occurrence)?;
        match c.occurrence.first() {
            None => Ok((None, c.frame())),
            Some(top) => {
                let top_t = d.occurrences.iter().find(|o| o.id == *top).ok_or_else(|| format!("occurrence {top:?} is not at the top level"))?.transform;
                let w = d.world_transform(&c.occurrence)?;
                Ok((Some(*top), top_t.inverse().mul(&w).mul(&c.frame())))
            }
        }
    };
    let world = |cur: &BTreeMap<OccurrenceId, Transform>, top: Option<OccurrenceId>, l: &Transform| match top {
        None => *l,
        Some(t) => cur[&t].mul(l),
    };
    let mut status: BTreeMap<MateId, MateStatus> = BTreeMap::new();
    let mut prepared = Vec::new();
    for m in &active {
        match (local(&m.a), local(&m.b)) {
            (Ok(a), Ok(b)) => {
                let ang = m.angle.eval(&vals);
                let dist = m.distance.eval(&vals);
                match (ang, dist) {
                    (Ok(ang), Ok(dist)) => prepared.push((*m, a, b, ang, dist)),
                    (Err(e), _) | (_, Err(e)) => {
                        status.insert(m.id, MateStatus::Unsolved(format!("drive value: {e}")));
                    }
                }
            }
            (Err(e), _) | (_, Err(e)) => {
                status.insert(m.id, MateStatus::Unsolved(e));
            }
        }
    }
    let mut moved = BTreeSet::new();
    loop {
        let mut progress = false;
        for (m, (ta, la), (tb, lb), ang, dist) in &prepared {
            if status.contains_key(&m.id) {
                continue;
            }
            let flip = if m.flip { flip_x() } else { Transform::IDENTITY };
            let (pa, pb) = (placed.contains(ta), placed.contains(tb));
            if ta == tb || (pa && pb) {
                let fa = world(&cur, *ta, la).mul(&flip);
                let fb = world(&cur, *tb, lb);
                let rel = fa.inverse().mul(&fb);
                let want = project_motion(m.kind, &rel, *ang, *dist);
                let (dl, da) = mismatch(&rel, &want);
                let st = if dl < 1e-6 && da < 1e-6 {
                    MateStatus::Redundant
                } else {
                    MateStatus::OverConstrained(format!("off by {dl:.3} mm / {da:.2}° (both sides are already fixed)"))
                };
                status.insert(m.id, st);
                continue;
            }
            if !pa && !pb {
                continue;
            }
            // move the unplaced side so its connector lands on the placed one
            let (fixed_t, fixed_l, mov_t, mov_l, a_moves) = if pa { (*ta, la, *tb, lb, false) } else { (*tb, lb, *ta, la, true) };
            let fixed_w = world(&cur, fixed_t, fixed_l);
            let mov_w = world(&cur, mov_t, mov_l);
            let target = if !a_moves {
                // B onto A: fb = fa·flip·M
                let base = fixed_w.mul(&flip);
                let rel = base.inverse().mul(&mov_w);
                base.mul(&project_motion(m.kind, &rel, *ang, *dist))
            } else {
                // A onto B: fa = fb·M⁻¹·flip⁻¹
                let rel = mov_w.mul(&flip).inverse().mul(&fixed_w);
                fixed_w.mul(&project_motion(m.kind, &rel, *ang, *dist).inverse()).mul(&flip.inverse())
            };
            let Some(top) = mov_t else { continue };
            let new_top = target.mul(&mov_l.inverse());
            let (dl, da) = mismatch(&new_top, &cur[&top]);
            if dl > 1e-9 || da > 1e-9 {
                moved.insert(top);
            }
            cur.insert(top, new_top);
            placed.insert(Some(top));
            status.insert(m.id, MateStatus::Solved);
            progress = true;
        }
        if !progress {
            break;
        }
    }
    for m in &d.mates {
        let st = if m.suppressed {
            MateStatus::Suppressed
        } else {
            status.remove(&m.id).unwrap_or_else(|| MateStatus::Unsolved("neither side is connected to a grounded occurrence".into()))
        };
        if let MateStatus::Unsolved(_) = st {
            for c in [&m.a, &m.b] {
                if let Some(t) = c.occurrence.first() {
                    if !placed.contains(&Some(*t)) && !rep.floating.contains(t) {
                        rep.floating.push(*t);
                    }
                }
            }
        }
        rep.mates.push(MateReport { mate: m.id, name: m.name.clone(), status: st, dof: m.kind.dof() });
    }
    rep.moved = moved.iter().copied().collect();
    let tf = moved.into_iter().map(|id| (id, cur[&id])).collect();
    (tf, rep)
}
