//! Manufacturing process profiles: first-class printer/process tolerances.
//!
//! A [`ProcessProfile`] says how much a process over- or under-builds, and how much
//! per-side allowance each [`FitClass`] needs. Dimensions carry an optional
//! [`Fit`] (role + class) and are resolved against the document's active profile:
//!
//! | role | resolved value |
//! |---|---|
//! | `Hole` (holes, slots, grooves, glands, pockets) | `x + 2·allow(class) + hole_comp(x)` |
//! | `Pin` (pins, tongues, lips, hooks, pegs) | `x − 2·allow(class) − 2·xy_comp` |
//! | `Gap` (a clearance between two faces, one side) | `x + allow(class)` |
//! | `Nominal` | `x` |
//!
//! Each part of a printed-to-printed mate takes its own share, so a `Slide` pin in
//! a `Slide` hole ends up with `2·allow(Slide)` per side plus compensation.
//! The same values are available inside expressions: `hole(x[, class])`,
//! `pin(x[, class])`, `gap([class])` and `tol(class)` (class defaults to `slide`).
//!
//! The built-in numbers are **starting points to calibrate** with the coupon from
//! [`calibration_coupon`] and [`ProcessProfile::from_calibration`].

use std::path::Path;

use glam::{DQuat, DVec3};
use occt::Shape;
use serde::{Deserialize, Serialize};

use crate::params::Values;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProcessKind {
    Fdm,
    Sla,
    Sls,
    Molded,
    Cnc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FitClass {
    Press,
    Snug,
    Slide,
    Loose,
}

impl FitClass {
    pub const ALL: [FitClass; 4] = [FitClass::Press, FitClass::Snug, FitClass::Slide, FitClass::Loose];
    pub fn name(self) -> &'static str {
        match self {
            FitClass::Press => "press",
            FitClass::Snug => "snug",
            FitClass::Slide => "slide",
            FitClass::Loose => "loose",
        }
    }
    pub fn from_name(s: &str) -> Option<FitClass> {
        FitClass::ALL.into_iter().find(|c| c.name() == s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FitRole {
    Hole,
    Pin,
    Gap,
    Nominal,
}

/// How a dimension is adjusted by the active process profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Fit {
    pub role: FitRole,
    pub class: FitClass,
}

impl Fit {
    pub const NOMINAL: Fit = Fit { role: FitRole::Nominal, class: FitClass::Slide };
    pub fn hole(class: FitClass) -> Fit {
        Fit { role: FitRole::Hole, class }
    }
    pub fn pin(class: FitClass) -> Fit {
        Fit { role: FitRole::Pin, class }
    }
    pub fn gap(class: FitClass) -> Fit {
        Fit { role: FitRole::Gap, class }
    }
}

/// Per-side allowance (mm) for each fit class. Negative = interference.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FitAllowances {
    pub press: f64,
    pub snug: f64,
    pub slide: f64,
    pub loose: f64,
}

impl FitAllowances {
    pub fn get(&self, c: FitClass) -> f64 {
        match c {
            FitClass::Press => self.press,
            FitClass::Snug => self.snug,
            FitClass::Slide => self.slide,
            FitClass::Loose => self.loose,
        }
    }
}

/// Added to hole diameters: `base + per_mm · d` (holes print undersize).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HoleComp {
    pub base: f64,
    #[serde(default)]
    pub per_mm: f64,
}

impl HoleComp {
    pub fn at(&self, d: f64) -> f64 {
        self.base + self.per_mm * d
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProcessProfile {
    pub name: String,
    pub kind: ProcessKind,
    pub fits: FitAllowances,
    pub hole_comp: HoleComp,
    /// Outer contours over-build by this much per side.
    pub xy_comp: f64,
    /// Elephant-foot growth of the first layer (per side).
    pub first_layer_comp: f64,
    /// Chamfer bottom edges by this much on export (counteracts elephant foot).
    #[serde(default)]
    pub first_layer_chamfer: Option<f64>,
    /// Teardrop (V-cut) holes for support-free horizontal holes.
    #[serde(default)]
    pub teardrop: TeardropSettings,
    #[serde(default)]
    pub notes: String,
    /// Parameters a new project starts with under this profile (see
    /// [`ProcessProfile::default_params`] for the built-in choices). Old files and
    /// presets without the field load as empty.
    #[serde(default)]
    pub params: Vec<ProfileParam>,
}

/// A parameter a profile pre-populates in new documents. `expr` may carry a
/// unit (`"1 deg"`); bare numbers are millimetres.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProfileParam {
    pub name: String,
    pub expr: String,
    #[serde(default)]
    pub comment: String,
}

fn pp(name: &str, expr: &str, comment: &str) -> ProfileParam {
    ProfileParam { name: name.into(), expr: expr.into(), comment: comment.into() }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum TeardropStyle {
    /// Two 45° tangent lines meeting at r·√2 above the centre.
    Pointed45,
    /// Pointed, truncated flat at `fraction · r` above the centre (bridged top).
    /// Useful range is √2/2 … √2 (1.0 is common).
    Flat(f64),
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TeardropSettings {
    pub enabled: bool,
    pub style: TeardropStyle,
    /// Apply when the hole axis is within this angle of horizontal (relative to the
    /// build-up direction).
    pub angle_threshold_deg: f64,
    /// Skip holes smaller than this diameter.
    pub min_diameter: f64,
}

impl Default for TeardropSettings {
    fn default() -> Self {
        TeardropSettings { enabled: false, style: TeardropStyle::Flat(1.0), angle_threshold_deg: 30.0, min_diameter: 2.0 }
    }
}

impl TeardropStyle {
    /// Material removed above a hole of radius r (cross-section area beyond the circle).
    pub fn cap_area(&self, r: f64) -> f64 {
        let pointed = r * r * (1.0 - std::f64::consts::FRAC_PI_4);
        match *self {
            TeardropStyle::Pointed45 => pointed,
            TeardropStyle::Flat(f) => {
                let h = (f.clamp(std::f64::consts::FRAC_1_SQRT_2, std::f64::consts::SQRT_2)) * r;
                pointed - (r * std::f64::consts::SQRT_2 - h).powi(2)
            }
        }
    }
}

/// One of the six cube faces of the model, as a direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CubeFace {
    PosX,
    NegX,
    PosY,
    NegY,
    PosZ,
    NegZ,
}

impl CubeFace {
    pub fn dir(self) -> DVec3 {
        match self {
            CubeFace::PosX => DVec3::X,
            CubeFace::NegX => DVec3::NEG_X,
            CubeFace::PosY => DVec3::Y,
            CubeFace::NegY => DVec3::NEG_Y,
            CubeFace::PosZ => DVec3::Z,
            CubeFace::NegZ => DVec3::NEG_Z,
        }
    }
}

/// Which model direction faces down onto the build plate, plus a rotation about
/// the plate normal in 90° steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BuildOrientation {
    pub down: CubeFace,
    #[serde(default)]
    pub quarter_turns: u8,
}

impl Default for BuildOrientation {
    fn default() -> Self {
        BuildOrientation { down: CubeFace::NegZ, quarter_turns: 0 }
    }
}

impl BuildOrientation {
    /// Rotation taking model coordinates to build coordinates (plate face → −Z).
    pub fn quat(&self) -> DQuat {
        let to_down = DQuat::from_rotation_arc(self.down.dir(), DVec3::NEG_Z);
        let to_down = if self.down == CubeFace::PosZ { DQuat::from_rotation_x(std::f64::consts::PI) } else { to_down };
        DQuat::from_rotation_z(std::f64::consts::FRAC_PI_2 * (self.quarter_turns % 4) as f64) * to_down
    }
    /// The build-up direction expressed in model coordinates.
    pub fn up_in_model(&self) -> DVec3 {
        -self.down.dir()
    }
}

/// Reserved keys used to pass the active profile through expression evaluation.
pub(crate) const KEY_ACTIVE: &str = "$profile";
pub(crate) fn key_allow(c: FitClass) -> String {
    format!("$allow.{}", c.name())
}
pub(crate) const KEY_HOLE_BASE: &str = "$hole_comp.base";
pub(crate) const KEY_HOLE_PER_MM: &str = "$hole_comp.per_mm";
pub(crate) const KEY_XY: &str = "$xy_comp";

impl ProcessProfile {
    pub fn fdm_generic() -> Self {
        ProcessProfile {
            name: "FDM 0.4 generic".into(),
            kind: ProcessKind::Fdm,
            fits: FitAllowances { press: -0.05, snug: 0.05, slide: 0.15, loose: 0.3 },
            hole_comp: HoleComp { base: 0.1, per_mm: 0.01 },
            xy_comp: 0.05,
            first_layer_comp: 0.15,
            first_layer_chamfer: Some(0.4),
            teardrop: TeardropSettings { enabled: true, ..Default::default() },
            notes: "Starting point for a 0.4 mm nozzle, 0.2 mm layers (PLA/PETG). Calibrate with the fit coupon.".into(),
            params: vec![
                // 4 perimeters of a 0.4 mm line: a wall made only of full perimeters
                pp("wall", "1.6", "4 perimeters x 0.4 mm nozzle"),
                // general part-to-part gap: the slide fit allowance above
                pp("clearance", "0.15", "gap between mating parts (slide fit)"),
                pp("layer", "0.2", "layer height"),
                pp("nozzle", "0.4", "nozzle diameter"),
                // below ~1 nozzle width a feature will not print as drawn
                pp("min_feature", "0.8", "smallest feature that prints (2 x nozzle)"),
            ],
        }
    }
    pub fn sla_generic() -> Self {
        ProcessProfile {
            name: "SLA generic".into(),
            kind: ProcessKind::Sla,
            fits: FitAllowances { press: -0.02, snug: 0.03, slide: 0.08, loose: 0.15 },
            hole_comp: HoleComp { base: 0.05, per_mm: 0.0 },
            xy_comp: 0.02,
            first_layer_comp: 0.0,
            first_layer_chamfer: None,
            teardrop: TeardropSettings::default(),
            notes: "Starting point for desktop MSLA resin printers. Calibrate with the fit coupon.".into(),
            params: vec![
                pp("wall", "1.5", "thin enough to cure cleanly, stiff enough to handle"),
                pp("clearance", "0.08", "gap between mating parts (slide fit)"),
                pp("layer", "0.05", "layer height"),
                pp("min_feature", "0.3", "smallest feature that prints"),
            ],
        }
    }
    pub fn molded_nominal() -> Self {
        ProcessProfile {
            name: "Injection molded (nominal)".into(),
            kind: ProcessKind::Molded,
            fits: FitAllowances { press: 0.0, snug: 0.0, slide: 0.0, loose: 0.0 },
            hole_comp: HoleComp::default(),
            xy_comp: 0.0,
            first_layer_comp: 0.0,
            first_layer_chamfer: None,
            teardrop: TeardropSettings::default(),
            notes: "Nominal geometry: no process compensation.".into(),
            params: vec![
                // uniform wall thickness avoids sink and warp; 2 mm suits ABS/PC/PP parts
                pp("wall", "2.0", "uniform wall (sink marks grow with thickness)"),
                // fits are nominal here, so the gap is a design choice, not a slide allowance
                pp("clearance", "0.1", "gap between mating parts"),
                pp("draft", "1 deg", "draft on faces along the pull direction"),
                pp("rib_ratio", "0.6", "rib thickness / wall (avoids sink opposite the rib)"),
            ],
        }
    }
    pub fn sls_generic() -> Self {
        ProcessProfile {
            name: "SLS nylon generic".into(),
            kind: ProcessKind::Sls,
            fits: FitAllowances { press: -0.05, snug: 0.05, slide: 0.15, loose: 0.3 },
            hole_comp: HoleComp { base: 0.1, per_mm: 0.0 },
            xy_comp: 0.05,
            first_layer_comp: 0.0,
            first_layer_chamfer: None,
            teardrop: TeardropSettings::default(),
            notes: "Starting point for powder-bed nylon (PA12). Self-supporting; no teardrops. Calibrate with the fit coupon.".into(),
            params: vec![
                pp("wall", "1.2", "minimum reliable wall for PA12"),
                pp("clearance", "0.15", "gap between mating parts (slide fit)"),
                pp("layer", "0.1", "layer height"),
                pp("min_feature", "0.8", "smallest feature that survives depowdering"),
            ],
        }
    }
    pub fn cnc_nominal() -> Self {
        ProcessProfile {
            name: "CNC machined (nominal)".into(),
            kind: ProcessKind::Cnc,
            fits: FitAllowances { press: 0.0, snug: 0.0, slide: 0.0, loose: 0.0 },
            hole_comp: HoleComp::default(),
            xy_comp: 0.0,
            first_layer_comp: 0.0,
            first_layer_chamfer: None,
            teardrop: TeardropSettings::default(),
            notes: "Nominal geometry: machining tolerances are handled by the shop.".into(),
            params: vec![
                pp("wall", "1.5", "thinnest wall that machines without chatter (aluminium)"),
                pp("clearance", "0.05", "gap between mating parts"),
                pp("tool_d", "3.175", "end mill diameter (1/8 in)"),
                // an internal corner cannot be sharper than the cutter
                pp("min_radius", "tool_d / 2", "smallest internal corner radius"),
            ],
        }
    }
    pub fn builtins() -> Vec<ProcessProfile> {
        vec![Self::fdm_generic(), Self::sla_generic(), Self::sls_generic(), Self::molded_nominal(), Self::cnc_nominal()]
    }

    pub fn allowance(&self, c: FitClass) -> f64 {
        self.fits.get(c)
    }

    /// Resolve a nominal value for a fit.
    pub fn resolve(&self, value: f64, fit: Fit) -> f64 {
        resolve_with(value, fit, |c| self.allowance(c), |d| self.hole_comp.at(d), self.xy_comp)
    }

    /// Put this profile's numbers into evaluation values (reserved `$` keys).
    pub fn inject(&self, v: &mut Values) {
        v.insert(KEY_ACTIVE.into(), 1.0);
        for c in FitClass::ALL {
            v.insert(key_allow(c), self.allowance(c));
        }
        v.insert(KEY_HOLE_BASE.into(), self.hole_comp.base);
        v.insert(KEY_HOLE_PER_MM.into(), self.hole_comp.per_mm);
        v.insert(KEY_XY.into(), self.xy_comp);
    }

    /// Load user presets (`*.json`, one profile each) from a directory.
    pub fn load_presets(dir: &Path) -> std::io::Result<Vec<ProcessProfile>> {
        let mut v = Vec::new();
        for e in std::fs::read_dir(dir)? {
            let p = e?.path();
            if p.extension().is_some_and(|x| x == "json") {
                let txt = std::fs::read_to_string(&p)?;
                v.push(serde_json::from_str(&txt).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{}: {e}", p.display())))?);
            }
        }
        v.sort_by(|a: &ProcessProfile, b| a.name.cmp(&b.name));
        Ok(v)
    }

    pub fn save_preset(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let file: String = self.name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
        std::fs::write(dir.join(format!("{file}.json")), serde_json::to_string_pretty(self).unwrap() + "\n")
    }

    /// Build a profile from coupon results (see [`calibration_coupon`]).
    pub fn from_calibration(r: &CalibrationResult) -> ProcessProfile {
        // The coupon pins are printed at nominal; a pin's contour over-builds by
        // xy_comp per side. Measured hole undersize gives hole_comp. The remaining
        // diametral offset that produced each feel is split between the two parts.
        let hole_comp = r.hole_measured.map(|m| (r.nominal - m).max(0.0)).unwrap_or(0.0);
        let xy = r.pin_measured.map(|m| ((m - r.nominal) / 2.0).max(0.0)).unwrap_or(0.0);
        // offset = hole_comp + 2·allow (hole) + 2·allow (pin share) + 2·xy  →  allow
        let allow = |offset: f64| (offset - hole_comp - 2.0 * xy) / 4.0;
        ProcessProfile {
            name: r.name.clone(),
            kind: r.kind,
            fits: FitAllowances {
                press: allow(r.press_offset),
                snug: allow(r.snug_offset),
                slide: allow(r.slide_offset),
                loose: allow(r.loose_offset),
            },
            hole_comp: HoleComp { base: hole_comp, per_mm: 0.0 },
            xy_comp: xy,
            first_layer_comp: r.first_layer_measured.unwrap_or(0.0),
            first_layer_chamfer: r.first_layer_measured.filter(|v| *v > 0.0).map(|v| (2.0 * v).max(0.2)),
            teardrop: TeardropSettings { enabled: r.kind == ProcessKind::Fdm, ..Default::default() },
            notes: format!("Calibrated from a {:.2} mm coupon.", r.nominal),
            params: Self::builtins().into_iter().find(|b| b.kind == r.kind).map(|b| b.params).unwrap_or_default(),
        }
    }
}

pub(crate) fn resolve_with(value: f64, fit: Fit, allow: impl Fn(FitClass) -> f64, hole_comp: impl Fn(f64) -> f64, xy: f64) -> f64 {
    match fit.role {
        FitRole::Hole => value + 2.0 * allow(fit.class) + hole_comp(value),
        FitRole::Pin => value - 2.0 * allow(fit.class) - 2.0 * xy,
        FitRole::Gap => value + allow(fit.class),
        FitRole::Nominal => value,
    }
}

/// Resolve against profile numbers injected into `vals`; nominal if none are present.
pub(crate) fn resolve_in(value: f64, fit: Fit, vals: &Values) -> f64 {
    if !vals.contains_key(KEY_ACTIVE) {
        return value;
    }
    let g = |k: &str| vals.get(k).copied().unwrap_or(0.0);
    resolve_with(value, fit, |c| g(&key_allow(c)), |d| g(KEY_HOLE_BASE) + g(KEY_HOLE_PER_MM) * d, g(KEY_XY))
}

/// What the user found on the printed coupon. Offsets are the coupon's diametral
/// hole-minus-pin steps (mm) that felt like each class.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CalibrationResult {
    pub name: String,
    pub kind: ProcessKind,
    pub nominal: f64,
    pub press_offset: f64,
    pub snug_offset: f64,
    pub slide_offset: f64,
    pub loose_offset: f64,
    /// Measured diameter of the nominal (offset 0) hole, if measured.
    #[serde(default)]
    pub hole_measured: Option<f64>,
    /// Measured diameter of a coupon pin, if measured.
    #[serde(default)]
    pub pin_measured: Option<f64>,
    /// Measured first-layer bulge per side, if measured.
    #[serde(default)]
    pub first_layer_measured: Option<f64>,
}

/// One cell of the calibration coupon.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CouponCell {
    pub index: usize,
    /// Hole diameter = nominal + offset.
    pub offset: f64,
    pub hole_center: DVec3,
    pub pin_center: DVec3,
    /// Number of index notches cut next to the cell (index + 1).
    pub notches: usize,
}

/// A fit-test plate: a row of holes `nominal + offset` and a matching row of pins at
/// `nominal`, with index notches (1..n) on the plate edge next to each cell. Printed
/// with the process to calibrate (no compensation applied).
pub fn calibration_coupon(nominal: f64, offsets: &[f64]) -> Result<(Shape, Vec<CouponCell>), occt::Error> {
    let n = offsets.len().max(1);
    let pitch = (nominal * 2.2).max(nominal + 6.0);
    let (t, pin_h) = (3.0, 6.0);
    let w = pitch * n as f64 + 4.0;
    let h = pitch * 2.0 + 6.0;
    let mut plate = Shape::make_box(DVec3::ZERO, DVec3::new(w, h, t))?;
    let mut holes = Vec::new();
    let mut pins = Vec::new();
    let mut cells = Vec::new();
    for (i, off) in offsets.iter().enumerate() {
        let x = 2.0 + pitch * (i as f64 + 0.5);
        let hc = DVec3::new(x, 3.0 + pitch * 0.5, 0.0);
        let pc = DVec3::new(x, 3.0 + pitch * 1.5, t);
        holes.push(Shape::cylinder(hc - DVec3::Z, DVec3::Z, (nominal + off) / 2.0, t + 2.0)?);
        pins.push(Shape::cylinder(pc, DVec3::Z, nominal / 2.0, pin_h)?);
        // notches on the front edge: i+1 small V-ish slots
        for k in 0..=i {
            let nx = x - pitch * 0.35 + k as f64 * 0.9;
            holes.push(Shape::make_box(DVec3::new(nx, -0.1, t - 0.6), DVec3::new(0.5, 1.1, 1.0))?);
        }
        cells.push(CouponCell { index: i, offset: *off, hole_center: hc, pin_center: pc, notches: i + 1 });
    }
    let hole_refs: Vec<&Shape> = holes.iter().collect();
    plate = plate.cut(&Shape::fuse_all(&hole_refs)?)?;
    let mut all = vec![&plate];
    all.extend(pins.iter());
    let coupon = Shape::fuse_all(&all)?.unify()?;
    Ok((coupon, cells))
}

/// Default ±0.05 mm steps for the coupon: −0.10 … +0.50.
pub fn default_coupon_offsets() -> Vec<f64> {
    (0..13).map(|i| -0.10 + 0.05 * i as f64).map(|v| (v * 1000.0).round() / 1000.0).collect()
}
