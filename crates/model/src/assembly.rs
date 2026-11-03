//! Assemblies: components, occurrences, in-context references, mates,
//! interference, BOM and assembly export.
//!
//! The document's root fields (`DocData::{params, sketches, features, imports}`) *are*
//! the root component. `DocData::components` is the table of the other component
//! definitions and `DocData::occurrences` the root's children. Each component
//! regenerates in its own local frame with its own regeneration cache; an occurrence
//! places a component in its parent's frame with a rigid [`Transform`].

use std::collections::BTreeMap;

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
    /// Revolute/Cylindrical drive value (degrees about z).
    #[serde(default = "zero")]
    pub angle: crate::params::Expr,
    /// Slider/Cylindrical drive value (mm along z), also the gap of Planar/Rigid.
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
