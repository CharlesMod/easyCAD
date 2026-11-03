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
    /// Its own geometry (a part node's shape) is one locked body, a reference body
    /// when `reference`; its sub-parts are child occurrences.
    Step {
        blob: String,
        node: u32,
        #[serde(default)]
        reference: bool,
    },
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
    step_trees: HashMap<String, Arc<occt::StepTree>>,
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
            Some(ComponentSource::Step { blob, node, reference }) => step_component(blob, *node, *reference, blobs, cache),
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

fn step_tree(blob: &str, blobs: &BlobStore, cache: &mut AssemblyCache) -> Result<Arc<occt::StepTree>, String> {
    if let Some(t) = cache.step_trees.get(blob) {
        return Ok(t.clone());
    }
    let data = blobs.get(blob).ok_or_else(|| format!("blob {blob} is missing"))?;
    let t = Arc::new(occt::StepTree::read(&String::from_utf8_lossy(&data)).map_err(|e| e.to_string())?);
    cache.step_trees.insert(blob.to_string(), t.clone());
    Ok(t)
}

/// A STEP node's own geometry as one body (no body for an assembly node).
fn step_component(blob: &str, node: u32, reference: bool, blobs: &BlobStore, cache: &mut AssemblyCache) -> RegenResult {
    use crate::regen::{Appearance, BodyGeom, FeatureReport, FeatureStatus, SubPart};
    let fid = FeatureId(0);
    let mut out = RegenResult { bodies: Vec::new(), reports: Vec::new(), recomputed: 0, reused: 0, process: Default::default() };
    let res = step_tree(blob, blobs, cache).and_then(|t| t.nodes.get(node as usize).cloned().ok_or_else(|| format!("STEP node {node} does not exist")));
    match res {
        Ok(n) => {
            if let Some(shape) = n.shape {
                let nf = shape.faces().map(|f| f.len()).unwrap_or(0);
                let app = Appearance { parts: vec![SubPart { name: n.name.clone(), color: n.color }], face_part: vec![0; nf] };
                out.bodies.push(Body { id: crate::feature::BodyId::of(fid), name: n.name, reference, part: None, geom: BodyGeom::with_appearance(shape, app) });
            }
        }
        Err(e) => out.reports.push(FeatureReport { feature: fid, status: FeatureStatus::Error(e), dressup: None, sketch: None, joint: None, cached: false, elapsed_ms: 0.0 }),
    }
    out
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
    /// Occurrences (any depth) whose transform was changed.
    pub moved: Vec<OccurrenceId>,
    /// Occurrences on unsolved mates that could not be placed.
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
                    if let Some(o) = d.occurrence_mut(*id) {
                        o.transform = *t;
                    }
                }
                Ok::<_, DocError>(())
            });
        }
        rep
    }
}

/// Solve without modifying the document: new local transforms of the occurrences
/// that moved, plus the report.
///
/// A propagation solver over occurrences at any depth:
/// - An occurrence is *fixed* when it is grounded, or when it is on no mate
///   connector path at all: unmated occurrences stay where they are and carry their
///   children. The root's own bodies are fixed.
/// - A connector is *placed* when every occurrence on its path is fixed or solved.
/// - A mate with one placed side moves the first unplaced occurrence on the other
///   side's path (so a mated sub-assembly moves rigidly, and a mated screw inside a
///   placed sub-assembly moves inside it), keeping its current position in the
///   mate's free directions. The new transform is written in that occurrence's
///   parent frame.
/// - A mate whose sides are both placed is checked (`Redundant` or
///   `OverConstrained`); mates never reached are `Unsolved`.
pub fn solve_mates(d: &DocData) -> (Vec<(OccurrenceId, Transform)>, MateSolve) {
    let mut rep = MateSolve::default();
    let vals = d.values().unwrap_or_default();
    // local transform of every occurrence, and its parent path
    let all = d.all_occurrences();
    let mut cur: BTreeMap<OccurrenceId, Transform> = all.iter().map(|(_, o, _)| (o.id, o.transform)).collect();
    let active: Vec<&Mate> = d.mates.iter().filter(|m| !m.suppressed).collect();
    let mated: BTreeSet<OccurrenceId> = active.iter().flat_map(|m| m.a.occurrence.iter().chain(&m.b.occurrence)).copied().collect();
    let mut placed: BTreeSet<OccurrenceId> = all.iter().filter(|(_, o, _)| o.grounded || !mated.contains(&o.id)).map(|(_, o, _)| o.id).collect();
    if !all.iter().any(|(_, o, _)| o.grounded) {
        let first = active.iter().find_map(|m| m.a.occurrence.first().or(m.b.occurrence.first()).copied());
        if let Some(f) = first {
            if !placed.contains(&f) {
                placed.insert(f);
                rep.notes.push(format!("nothing is grounded: `{}` was held fixed", d.occurrence(f).map(|o| o.name.as_str()).unwrap_or("?")));
            }
        }
    }
    let world_of = |cur: &BTreeMap<OccurrenceId, Transform>, path: &[OccurrenceId]| path.iter().fold(Transform::IDENTITY, |t, o| t.mul(&cur[o]));
    let mut status: BTreeMap<MateId, MateStatus> = BTreeMap::new();
    let mut prepared = Vec::new();
    for m in &active {
        let bad = [&m.a, &m.b].into_iter().find_map(|c| d.component_at(&c.occurrence).err());
        if let Some(e) = bad {
            status.insert(m.id, MateStatus::Unsolved(e));
            continue;
        }
        match (m.angle.eval(&vals), m.distance.eval(&vals)) {
            (Ok(a), Ok(dist)) => prepared.push((*m, a, dist)),
            (Err(e), _) | (_, Err(e)) => {
                status.insert(m.id, MateStatus::Unsolved(format!("drive value: {e}")));
            }
        }
    }
    let mut moved = BTreeSet::new();
    loop {
        let mut progress = false;
        for (m, ang, dist) in &prepared {
            if status.contains_key(&m.id) {
                continue;
            }
            let flip = if m.flip { flip_x() } else { Transform::IDENTITY };
            let first_unplaced = |p: &[OccurrenceId]| p.iter().position(|o| !placed.contains(o));
            let (ua, ub) = (first_unplaced(&m.a.occurrence), first_unplaced(&m.b.occurrence));
            let fa = world_of(&cur, &m.a.occurrence).mul(&m.a.frame());
            let fb = world_of(&cur, &m.b.occurrence).mul(&m.b.frame());
            match (ua, ub) {
                (None, None) => {
                    let base = fa.mul(&flip);
                    let rel = base.inverse().mul(&fb);
                    let want = project_motion(m.kind, &rel, *ang, *dist);
                    let (dl, da) = mismatch(&rel, &want);
                    let st = if dl < 1e-6 && da < 1e-6 {
                        MateStatus::Redundant
                    } else {
                        MateStatus::OverConstrained(format!("off by {dl:.3} mm / {da:.2}° (both sides are already fixed)"))
                    };
                    status.insert(m.id, st);
                }
                (Some(_), Some(_)) => {}
                (fixed_a, _) => {
                    // move the first unplaced occurrence of the other side
                    let b_moves = fixed_a.is_none();
                    let (mov_conn, k) = if b_moves { (&m.b, ub.unwrap()) } else { (&m.a, ua.unwrap()) };
                    let (fixed_w, mov_w) = if b_moves { (fa, fb) } else { (fb, fa) };
                    let target = if b_moves {
                        let base = fixed_w.mul(&flip);
                        let rel = base.inverse().mul(&mov_w);
                        base.mul(&project_motion(m.kind, &rel, *ang, *dist))
                    } else {
                        let rel = mov_w.mul(&flip).inverse().mul(&fixed_w);
                        fixed_w.mul(&project_motion(m.kind, &rel, *ang, *dist).inverse()).mul(&flip.inverse())
                    };
                    let path = &mov_conn.occurrence;
                    let mover = path[k];
                    let parent_w = world_of(&cur, &path[..k]);
                    // connector frame relative to the mover (descendants fixed relative to it)
                    let rel = world_of(&cur, &path[k + 1..]).mul(&mov_conn.frame());
                    let new_local = parent_w.inverse().mul(&target).mul(&rel.inverse());
                    let (dl, da) = mismatch(&new_local, &cur[&mover]);
                    if dl > 1e-9 || da > 1e-9 {
                        moved.insert(mover);
                    }
                    cur.insert(mover, new_local);
                    placed.insert(mover);
                    status.insert(m.id, MateStatus::Solved);
                    progress = true;
                }
            }
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
                for o in &c.occurrence {
                    if !placed.contains(o) && !rep.floating.contains(o) {
                        rep.floating.push(*o);
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

// ---------------------------------------------------------------- interference

/// One placed body named for reports.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InstanceRef {
    pub path: OccPath,
    pub body: crate::feature::BodyId,
    /// "Base:1 / Body".
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Interference {
    pub a: InstanceRef,
    pub b: InstanceRef,
    /// Volume of the common solid (mm³).
    pub volume: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct InterferenceReport {
    pub pairs: Vec<Interference>,
    /// Pairs whose bounding boxes overlapped and were intersected exactly.
    pub tested: usize,
    /// Pairs rejected by the bounding-box test.
    pub culled: usize,
    /// Pairs the kernel could not intersect (with the reason).
    pub failed: Vec<(InstanceRef, InstanceRef, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InterferenceOptions {
    /// Include reference bodies (PCBs, library parts). Default true.
    pub include_reference: bool,
    /// Include hidden occurrences. Default false.
    pub include_hidden: bool,
    /// Smallest common volume reported (mm³). Default 1e-3.
    pub min_volume: f64,
}

impl Default for InterferenceOptions {
    fn default() -> Self {
        InterferenceOptions { include_reference: true, include_hidden: false, min_volume: 1e-3 }
    }
}

/// Interference between instances of different occurrences: bounding-box cull, then
/// the exact common volume (pairs run on parallel threads).
pub fn interference(d: &DocData, r: &AssemblyResult, opts: &InterferenceOptions) -> InterferenceReport {
    let inst: Vec<&Instance> = r.instances.iter().filter(|i| (opts.include_hidden || i.visible) && (opts.include_reference || !i.body.reference)).collect();
    let mut placed = Vec::new();
    let mut rep = InterferenceReport::default();
    for i in &inst {
        let rf = InstanceRef { path: i.path.clone(), body: i.body.id, label: i.label(d) };
        match i.world_shape().and_then(|s| s.bbox().map(|b| (s, b))) {
            Ok((s, b)) => placed.push((rf, s, b)),
            Err(e) => rep.failed.push((rf.clone(), rf, e.to_string())),
        }
    }
    let mut todo = Vec::new();
    for x in 0..placed.len() {
        for y in x + 1..placed.len() {
            if placed[x].0.path == placed[y].0.path {
                continue;
            }
            let (a, b) = (&placed[x].2, &placed[y].2);
            let overlap = (0..3).all(|k| a.min[k] < b.max[k] - 1e-6 && b.min[k] < a.max[k] - 1e-6);
            if overlap {
                todo.push((x, y));
            } else {
                rep.culled += 1;
            }
        }
    }
    rep.tested = todo.len();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 8);
    let chunk = todo.len().div_ceil(threads).max(1);
    let placed = &placed;
    let results: Vec<(usize, usize, Result<f64, String>)> = std::thread::scope(|sc| {
        let hs: Vec<_> = todo
            .chunks(chunk)
            .map(|ch| {
                sc.spawn(move || {
                    ch.iter()
                        .map(|&(x, y)| {
                            let v = placed[x].1.common(&placed[y].1).and_then(|c| if c.count(occt::ShapeKind::Solid) == 0 { Ok(0.0) } else { c.volume() });
                            (x, y, v.map_err(|e| e.to_string()))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    for (x, y, v) in results {
        match v {
            Ok(v) if v > opts.min_volume => rep.pairs.push(Interference { a: placed[x].0.clone(), b: placed[y].0.clone(), volume: v }),
            Ok(_) => {}
            Err(e) => rep.failed.push((placed[x].0.clone(), placed[y].0.clone(), e)),
        }
    }
    rep
}

impl Document {
    /// Interference between the occurrences of the regenerated assembly.
    pub fn interference(&mut self, opts: &InterferenceOptions) -> InterferenceReport {
        let r = self.regenerate_assembly().clone();
        interference(&self.data, &r, opts)
    }
}

// ---------------------------------------------------------------- BOM

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum BomKind {
    /// Made in house with this process profile.
    Made { process: String },
    /// Bought (library part, PCB, other STEP reference).
    Purchased { source: String, part: String, size: String, vendor: Option<String>, part_number: Option<String> },
    /// A sub-assembly with no bodies of its own.
    Assembly,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AsmBomRow {
    /// Depth in the indented BOM (0 = directly in the root); 0 in the flat BOM.
    pub level: usize,
    /// The component (None for library parts inserted by an `InsertPart` feature).
    pub component: Option<ComponentId>,
    pub name: String,
    /// Quantity per parent (indented) or 0 (flat).
    pub qty: usize,
    /// Rolled-up quantity in the whole assembly.
    pub total: usize,
    pub kind: BomKind,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AssemblyBom {
    pub indented: Vec<AsmBomRow>,
    /// One row per distinct component / part, sorted made-first then by name.
    pub flat: Vec<AsmBomRow>,
}

fn part_kind(p: &PartRef) -> BomKind {
    BomKind::Purchased { source: p.source.clone(), part: p.id.clone(), size: p.size.clone(), vendor: p.vendor.clone(), part_number: p.part_number.clone() }
}

/// BOM of the occurrence tree (occurrences with `in_bom`), quantities rolled up.
pub fn bom(d: &DocData, r: &AssemblyResult) -> AssemblyBom {
    let mut out = AssemblyBom::default();
    fn rec(d: &DocData, r: &AssemblyResult, comp: ComponentId, level: usize, mult: usize, out: &mut AssemblyBom) {
        if level > 64 {
            return;
        }
        // library parts placed by features inside this component
        if d.component(comp).is_none_or(|c| c.purchased.is_none()) {
            let mut parts: Vec<(PartRef, usize)> = Vec::new();
            if let Some(cr) = r.component(comp) {
                for b in &cr.regen.bodies {
                    if let Some(pu) = &b.part {
                        match parts.iter_mut().find(|(p, _)| p.source == pu.part.source && p.id == pu.part.id && p.size == pu.part.size) {
                            Some(e) => e.1 += 1,
                            None => parts.push((pu.part.clone(), 1)),
                        }
                    }
                }
            }
            for (p, n) in parts {
                out.indented.push(AsmBomRow { level, component: None, name: format!("{} {}", p.name, p.size), qty: n, total: n * mult, kind: part_kind(&p) });
            }
        }
        let Some(kids) = d.children_of(comp) else { return };
        let mut groups: Vec<(ComponentId, usize)> = Vec::new();
        for o in kids.iter().filter(|o| o.in_bom) {
            match groups.iter_mut().find(|g| g.0 == o.component) {
                Some(g) => g.1 += 1,
                None => groups.push((o.component, 1)),
            }
        }
        for (cid, n) in groups {
            let Some(c) = d.component(cid) else { continue };
            let has_solid = r.component(cid).is_some_and(|cr| cr.regen.bodies.iter().any(|b| !b.reference));
            let kind = match &c.purchased {
                Some(p) => part_kind(p),
                None if !has_solid && !c.children.is_empty() => BomKind::Assembly,
                None => BomKind::Made { process: c.process.clone().unwrap_or_else(|| d.active_profile.clone()) },
            };
            let purchased = c.purchased.is_some();
            out.indented.push(AsmBomRow { level, component: Some(cid), name: c.name.clone(), qty: n, total: n * mult, kind });
            if !purchased {
                rec(d, r, cid, level + 1, n * mult, out);
            }
        }
    }
    rec(d, r, ComponentId::ROOT, 0, 1, &mut out);
    for row in &out.indented {
        let same = |f: &AsmBomRow| match (&row.component, &f.component) {
            (Some(a), Some(b)) => a == b,
            (None, None) => row.kind == f.kind,
            _ => false,
        };
        match out.flat.iter_mut().find(|f| same(f)) {
            Some(f) => f.total += row.total,
            None => out.flat.push(AsmBomRow { level: 0, qty: 0, ..row.clone() }),
        }
    }
    let rank = |k: &BomKind| match k {
        BomKind::Made { .. } => 0,
        BomKind::Assembly => 1,
        BomKind::Purchased { .. } => 2,
    };
    out.flat.sort_by(|a, b| (rank(&a.kind), &a.name).cmp(&(rank(&b.kind), &b.name)));
    out
}

impl Document {
    /// Indented and flat BOM of the assembly (see [`bom`]).
    pub fn bom_tree(&mut self) -> AssemblyBom {
        let r = self.regenerate_assembly().clone();
        bom(&self.data, &r)
    }
}

// ---------------------------------------------------------------- STEP import / export

impl Document {
    /// Import a STEP file as components: one component per XCAF product (shared by
    /// all its instances), sub-parts as child occurrences with the file's placements,
    /// placed in `parent` at the origin. `reference` (a PCB, a bought part): bodies
    /// are reference bodies and the top component is purchased (its BOM line). STEP
    /// components are always locked. Returns the top component and its occurrence.
    pub fn import_step_assembly(&mut self, name: &str, data: Vec<u8>, parent: ComponentId, reference: bool) -> Result<(ComponentId, OccurrenceId), DocError> {
        let tree = occt::StepTree::read(&String::from_utf8_lossy(&data)).map_err(|e| DocError::Other(e.to_string()))?;
        let blob = self.blobs.put(data);
        self.asm.step_trees.insert(blob.clone(), Arc::new(tree.clone()));
        let (top, occ) = self.edit(&format!("Import {name}"), |d| {
            let mut ids: HashMap<usize, ComponentId> = HashMap::new();
            for k in 0..tree.nodes.len() {
                ids.insert(k, ComponentId(d.alloc()));
            }
            let mut comps = Vec::new();
            for (k, n) in tree.nodes.iter().enumerate() {
                let mut children = Vec::new();
                for (j, l) in n.children.iter().enumerate() {
                    let child = &tree.nodes[l.child];
                    let nm = if l.name.is_empty() || l.name.starts_with("=>") { format!("{}:{}", child.name, j + 1) } else { l.name.clone() };
                    children.push(Occurrence {
                        id: OccurrenceId(d.alloc()),
                        component: ids[&l.child],
                        name: nm,
                        transform: Transform::from_rows(&l.matrix),
                        visible: true,
                        grounded: false,
                        explode: DVec3::ZERO,
                        in_bom: true,
                    });
                }
                let nm = if n.name.is_empty() { format!("{name} part {}", k + 1) } else { n.name.clone() };
                comps.push(Component {
                    id: ids[&k],
                    name: nm,
                    children,
                    source: ComponentSource::Step { blob: blob.clone(), node: k as u32, reference },
                    locked: true,
                    ..Default::default()
                });
            }
            // the top: the single root product, or a wrapper around several
            let top = if tree.roots.len() == 1 {
                ids[&tree.roots[0]]
            } else {
                let id = ComponentId(d.alloc());
                let children = tree
                    .roots
                    .iter()
                    .map(|r| Occurrence {
                        id: OccurrenceId(d.alloc()),
                        component: ids[r],
                        name: format!("{}:1", tree.nodes[*r].name),
                        transform: Transform::IDENTITY,
                        visible: true,
                        grounded: false,
                        explode: DVec3::ZERO,
                        in_bom: true,
                    })
                    .collect();
                comps.push(Component { id, name: name.into(), children, locked: true, ..Default::default() });
                id
            };
            if let Some(c) = comps.iter_mut().find(|c| c.id == top) {
                c.name = name.into();
                if reference {
                    c.purchased = Some(PartRef {
                        source: "STEP".into(),
                        id: name.into(),
                        size: String::new(),
                        name: name.into(),
                        part_number: None,
                        vendor: None,
                        blob: Some(blob.clone()),
                        thread: None,
                    });
                }
            }
            d.components.extend(comps);
            d.components.sort_by_key(|c| c.id);
            let occ = OccurrenceId(d.alloc());
            let o = Occurrence { id: occ, component: top, name: format!("{name}:1"), transform: Transform::IDENTITY, visible: true, grounded: false, explode: DVec3::ZERO, in_bom: true };
            d.children_mut(parent).ok_or_else(|| DocError::Other(format!("component {parent:?} does not exist")))?.push(o);
            Ok::<_, DocError>((top, occ))
        })?;
        Ok((top, occ))
    }
}

/// What `Document::export_assembly` writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssemblyFormat {
    /// One STEP file with the product structure (XCAF): components as assemblies,
    /// bodies as parts, occurrences as placed instances with names and colours.
    Step,
    /// One 3MF with an object per component (its solid bodies) and a build item per
    /// placed occurrence (with its transform).
    ThreeMf,
    /// `path` is a directory: one binary STL per component with solids, in the
    /// component's own frame, named after the component.
    StlPerComponent,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AssemblyExportOptions {
    /// STEP: include reference bodies (PCBs, library parts). STL/3MF never do.
    pub include_reference: bool,
    /// Include hidden occurrences.
    pub include_hidden: bool,
}

fn file_safe(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' }).collect::<String>().trim().to_string()
}

impl Document {
    /// Export the whole assembly. Returns the files written.
    pub fn export_assembly(&mut self, format: AssemblyFormat, path: &std::path::Path, opts: &AssemblyExportOptions) -> Result<Vec<std::path::PathBuf>, DocError> {
        let r = self.regenerate_assembly().clone();
        let d = &self.data;
        let other = |e: String| DocError::Other(e);
        let keep_body = |b: &Body, step: bool| if step { opts.include_reference || !b.reference } else { !b.reference };
        let step = format == AssemblyFormat::Step;
        // components that end up with something to export (bodies or exported children)
        let mut memo: HashMap<ComponentId, bool> = HashMap::new();
        fn has_content(d: &DocData, r: &AssemblyResult, c: ComponentId, keep: &dyn Fn(&Body) -> bool, hidden: bool, memo: &mut HashMap<ComponentId, bool>, depth: usize) -> bool {
            if let Some(v) = memo.get(&c) {
                return *v;
            }
            let own = r.component(c).is_some_and(|cr| cr.regen.bodies.iter().any(keep));
            let kids = depth < 64
                && d.children_of(c).is_some_and(|k| k.iter().any(|o| (hidden || o.visible) && has_content(d, r, o.component, keep, hidden, memo, depth + 1)));
            memo.insert(c, own || kids);
            own || kids
        }
        let keep = |b: &Body| keep_body(b, step);
        match format {
            AssemblyFormat::Step => {
                let mut tree = occt::StepTree::default();
                let mut node_of: HashMap<ComponentId, usize> = HashMap::new();
                #[allow(clippy::too_many_arguments)] // recursive helper over the export state
                fn build(
                    d: &DocData,
                    r: &AssemblyResult,
                    c: ComponentId,
                    keep: &dyn Fn(&Body) -> bool,
                    hidden: bool,
                    tree: &mut occt::StepTree,
                    node_of: &mut HashMap<ComponentId, usize>,
                    memo: &mut HashMap<ComponentId, bool>,
                    depth: usize,
                ) -> usize {
                    if let Some(n) = node_of.get(&c) {
                        return *n;
                    }
                    let me = tree.nodes.len();
                    tree.nodes.push(occt::StepNode { name: d.component_name(c), ..Default::default() });
                    node_of.insert(c, me);
                    let ident = Transform::IDENTITY.to_rows();
                    if let Some(cr) = r.component(c) {
                        for b in cr.regen.bodies.iter().filter(|b| keep(b)) {
                            let color = b.appearance().and_then(|a| a.parts.first()).and_then(|p| p.color);
                            let leaf = tree.nodes.len();
                            tree.nodes.push(occt::StepNode { name: b.name.clone(), color, shape: Some(b.shape().clone()), children: Vec::new() });
                            tree.nodes[me].children.push(occt::StepLink { child: leaf, name: b.name.clone(), matrix: ident });
                        }
                    }
                    for o in d.children_of(c).unwrap_or(&[]) {
                        if !(hidden || o.visible) || !has_content(d, r, o.component, keep, hidden, memo, depth + 1) || depth > 64 {
                            continue;
                        }
                        let child = build(d, r, o.component, keep, hidden, tree, node_of, memo, depth + 1);
                        tree.nodes[me].children.push(occt::StepLink { child, name: o.name.clone(), matrix: o.transform.to_rows() });
                    }
                    me
                }
                if !has_content(d, &r, ComponentId::ROOT, &keep, opts.include_hidden, &mut memo, 0) {
                    return Err(DocError::Other("nothing to export".into()));
                }
                let root = build(d, &r, ComponentId::ROOT, &keep, opts.include_hidden, &mut tree, &mut node_of, &mut memo, 0);
                tree.roots = vec![root];
                let s = tree.to_step_string().map_err(|e| other(e.to_string()))?;
                std::fs::write(path, s)?;
                Ok(vec![path.to_path_buf()])
            }
            AssemblyFormat::ThreeMf => {
                let mut objects: Vec<(String, Vec<&Body>)> = Vec::new();
                let mut obj_of: HashMap<ComponentId, usize> = HashMap::new();
                let mut items: Vec<(usize, [f64; 12])> = Vec::new();
                for i in r.instances.iter().filter(|i| (opts.include_hidden || i.visible) && keep(&i.body)) {
                    let k = *obj_of.entry(i.component).or_insert_with(|| {
                        let cr = r.component(i.component).unwrap();
                        objects.push((d.component_name(i.component), cr.regen.bodies.iter().filter(|b| keep(b)).collect()));
                        objects.len() - 1
                    });
                    let m = i.transform.to_rows();
                    if !items.iter().any(|(o, mm)| *o == k && *mm == m) {
                        items.push((k, m));
                    }
                }
                crate::export::write_3mf_assembly(&objects, &items, path).map_err(|e| other(e.to_string()))?;
                Ok(vec![path.to_path_buf()])
            }
            AssemblyFormat::StlPerComponent => {
                std::fs::create_dir_all(path)?;
                let mut written = Vec::new();
                let mut used: HashMap<String, usize> = HashMap::new();
                let mut done: BTreeSet<ComponentId> = BTreeSet::new();
                for i in r.instances.iter().filter(|i| (opts.include_hidden || i.visible) && keep(&i.body)) {
                    if !done.insert(i.component) {
                        continue;
                    }
                    let cr = r.component(i.component).unwrap();
                    let bodies: Vec<&Body> = cr.regen.bodies.iter().filter(|b| keep(b)).collect();
                    let base = file_safe(&d.component_name(i.component));
                    let n = used.entry(base.clone()).or_insert(0);
                    *n += 1;
                    let fname = if *n == 1 { format!("{base}.stl") } else { format!("{base} ({n}).stl") };
                    let p = path.join(fname);
                    crate::export::write_stl(&bodies, &p).map_err(|e| other(e.to_string()))?;
                    written.push(p);
                }
                if written.is_empty() {
                    return Err(DocError::Other("nothing to export".into()));
                }
                Ok(written)
            }
        }
    }
}

// ---------------------------------------------------------------- linked components

/// State of a linked component against its file.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum LinkStatus {
    NotLinked,
    UpToDate,
    /// The file changed since the content was last copied in.
    OutOfDate,
    /// The file cannot be read (path, reason).
    Missing(String),
}

/// `target` relative to `base` (both folders), with `..` where needed.
pub fn relative_path(base: &std::path::Path, target: &std::path::Path) -> String {
    use std::path::Component as C;
    let abs = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (b, t) = (abs(base), abs(target));
    let bc: Vec<C> = b.components().collect();
    let tc: Vec<C> = t.components().collect();
    let common = bc.iter().zip(&tc).take_while(|(x, y)| x == y).count();
    let mut parts: Vec<String> = vec!["..".into(); bc.len() - common];
    parts.extend(tc[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

/// Read a `.cwk` folder for linking: its data, the hash of its `doc.json`, blobs.
fn read_linked(dir: &std::path::Path) -> Result<(DocData, String, Vec<Vec<u8>>), DocError> {
    let json = std::fs::read_to_string(dir.join(crate::document::DOC_FILE))?;
    let hash = crate::document::sha256_hex(json.as_bytes());
    let src = Document::load_from_dir(dir)?;
    if !src.data.components.is_empty() || !src.data.occurrences.is_empty() {
        // TODO: link files that are assemblies themselves (remap their component ids).
        return Err(DocError::Other("linking a file that is itself an assembly is not supported yet; link its parts".into()));
    }
    let blobs = src.blob_refs().iter().filter_map(|h| src.blobs.get(h)).map(|b| (*b).clone()).collect();
    Ok((src.data, hash, blobs))
}

fn copy_linked(c: &mut Component, src: &DocData) {
    c.params = src.params.clone();
    c.sketches = src.sketches.clone();
    c.features = src.features.clone();
    c.imports = src.imports.clone();
}

impl Document {
    /// Link an external `.cwk` folder as a locked component (its root content is
    /// copied in as a cache; its params become the component's local params). The
    /// path is stored relative to `host_dir` (the folder the host document is saved
    /// in). Place it with `add_occurrence`.
    pub fn link_component(&mut self, cwk: &std::path::Path, host_dir: &std::path::Path) -> Result<ComponentId, DocError> {
        let (src, hash, blobs) = read_linked(cwk)?;
        for b in blobs {
            self.blobs.put(b);
        }
        let path = relative_path(host_dir, cwk);
        self.edit(&format!("Link {}", src.name), |d| {
            let id = ComponentId(d.alloc());
            let mut c = Component { id, name: src.name.clone(), source: ComponentSource::Linked { path, hash }, locked: true, ..Default::default() };
            copy_linked(&mut c, &src);
            // ids inside the linked content stay unique against later allocations
            d.next_id = d.next_id.max(src.next_id);
            d.components.push(c);
            Ok::<_, DocError>(id)
        })
    }

    pub fn link_status(&self, id: ComponentId, host_dir: &std::path::Path) -> LinkStatus {
        let Some(ComponentSource::Linked { path, hash }) = self.data.component(id).map(|c| &c.source) else { return LinkStatus::NotLinked };
        let p = host_dir.join(path);
        match std::fs::read_to_string(p.join(crate::document::DOC_FILE)) {
            Ok(j) if crate::document::sha256_hex(j.as_bytes()) == *hash => LinkStatus::UpToDate,
            Ok(_) => LinkStatus::OutOfDate,
            Err(e) => LinkStatus::Missing(format!("{}: {e}", p.display())),
        }
    }

    /// Re-copy a linked component's content from its file (undoable).
    pub fn update_link(&mut self, id: ComponentId, host_dir: &std::path::Path) -> Result<(), DocError> {
        let Some(ComponentSource::Linked { path, .. }) = self.data.component(id).map(|c| c.source.clone()) else {
            return Err(DocError::Other("not a linked component".into()));
        };
        let (src, hash, blobs) = read_linked(&host_dir.join(&path))?;
        for b in blobs {
            self.blobs.put(b);
        }
        self.edit(&format!("Update link {}", src.name), |d| {
            d.next_id = d.next_id.max(src.next_id);
            let c = d.component_mut(id).ok_or_else(|| DocError::Other("no such component".into()))?;
            copy_linked(c, &src);
            c.source = ComponentSource::Linked { path, hash };
            Ok(())
        })
    }

    /// Break a link: the cached content becomes a normal, editable component.
    pub fn break_link(&mut self, id: ComponentId) -> Result<(), DocError> {
        self.edit("Break link", |d| {
            let c = d.component_mut(id).ok_or_else(|| DocError::Other("no such component".into()))?;
            if !matches!(c.source, ComponentSource::Linked { .. }) {
                return Err(DocError::Other("not a linked component".into()));
            }
            c.source = ComponentSource::Native;
            c.locked = false;
            Ok(())
        })
    }

    // ------------------------------------------------------------ structure helpers

    /// Turn a flat document into an assembly: the root's sketches, features and
    /// imports move into a new component (document params stay shared), placed once
    /// at the origin. Root feature ids are kept.
    pub fn move_root_into_component(&mut self, name: &str) -> Result<(ComponentId, OccurrenceId), DocError> {
        self.edit(&format!("Make component {name}"), |d| {
            if d.features.iter().any(|f| matches!(f.kind, FeatureKind::ContextRef { .. })) {
                return Err(DocError::Other("the root has in-context references; move them first".into()));
            }
            let id = ComponentId(d.alloc());
            let c = Component {
                id,
                name: name.into(),
                sketches: std::mem::take(&mut d.sketches),
                features: std::mem::take(&mut d.features),
                imports: std::mem::take(&mut d.imports),
                ..Default::default()
            };
            d.components.push(c);
            let occ = OccurrenceId(d.alloc());
            d.occurrences.push(Occurrence { id: occ, component: id, name: format!("{name}:1"), transform: Transform::IDENTITY, visible: true, grounded: true, explode: DVec3::ZERO, in_bom: true });
            Ok((id, occ))
        })
    }

    /// Split / PartingJoint output as components: for the `Split` or `PartingJoint`
    /// feature `feature` of the component at `source`, make one new component per
    /// half (named after the bodies, e.g. "Body" and "Lid"), each built in context
    /// from that half (`ContextRef`, so they follow every edit of the source), placed
    /// in the root where the source is. The source occurrence is hidden and left out
    /// of the BOM (it is the design master). `names` overrides the component names.
    /// Returns (lower, upper).
    pub fn split_to_components(&mut self, source: &[OccurrenceId], feature: FeatureId, names: Option<(&str, &str)>) -> Result<[(ComponentId, OccurrenceId); 2], DocError> {
        let err = |m: String| DocError::Other(m);
        if source.is_empty() {
            return Err(err("the source must be an occurrence; use move_root_into_component first".into()));
        }
        let comp = self.data.component_at(source).map_err(err)?;
        let world = self.data.world_transform(source).map_err(err)?;
        let f = self.data.component(comp).and_then(|c| c.features.iter().find(|f| f.id == feature)).cloned().ok_or_else(|| err(format!("feature {feature:?} is not in that component")))?;
        let target = match &f.kind {
            FeatureKind::Split { body, .. } | FeatureKind::PartingJoint { body, .. } => *body,
            _ => return Err(err(format!("`{}` is not a split or parting joint", f.name))),
        };
        let r = self.component_result(comp).ok_or_else(|| err("the source did not regenerate".into()))?;
        if let Some(e) = r.report(feature).and_then(|rep| match &rep.status {
            crate::regen::FeatureStatus::Error(e) => Some(e.clone()),
            _ => None,
        }) {
            return Err(err(format!("`{}` failed: {e}", f.name)));
        }
        let upper = r.body_by_id(crate::feature::BodyId::of(feature)).ok_or_else(|| err("the split made no upper body".into()))?.name.clone();
        let lower = match target {
            Some(t) => r.body_by_id(t).map(|b| b.name.clone()),
            None => r.bodies.iter().find(|b| !b.reference && b.name != upper).map(|b| b.name.clone()),
        }
        .ok_or_else(|| err("cannot find the lower body".into()))?;
        let src = source.to_vec();
        let cnames = match names {
            Some((a, b)) => [a.to_string(), b.to_string()],
            None => [lower.clone(), upper.clone()],
        };
        self.edit(&format!("Split {} into components", f.name), |d| {
            let mut out = Vec::new();
            for (body, cname) in [&lower, &upper].into_iter().zip(&cnames) {
                let id = ComponentId(d.alloc());
                let fid = FeatureId(d.alloc());
                let feat = Feature {
                    id: fid,
                    name: format!("{body} (in context)"),
                    suppressed: false,
                    kind: FeatureKind::ContextRef { source: src.clone(), body: Some(body.clone()), op: crate::feature::BoolOp::NewBody, target: None, offset: zero(), reference: false },
                };
                d.components.push(Component { id, name: cname.clone(), features: vec![feat], ..Default::default() });
                let occ = OccurrenceId(d.alloc());
                d.occurrences.push(Occurrence { id: occ, component: id, name: format!("{cname}:1"), transform: world, visible: true, grounded: false, explode: DVec3::ZERO, in_bom: true });
                out.push((id, occ));
            }
            let o = d.occurrence_mut(src[src.len() - 1]).ok_or_else(|| err("source occurrence vanished".into()))?;
            o.visible = false;
            o.in_bom = false;
            Ok([out[0], out[1]])
        })
    }
}
