//! Parts library: a [`PartSource`] trait with a built-in parametric catalog
//! (metric + imperial fasteners, inserts, standoffs, magnets, feet) and a local
//! STEP folder source.
//!
//! Dimension tables live in `data/parts/*.json` (one table per file, each citing its
//! standard) and are interpreted by a handful of generic generators, so adding a
//! size or a series is data-only. Imperial tables stay inch-native; conversion to
//! mm happens only when geometry or a [`HoleSpec`] is produced.
//!
//! Parts are built in a local frame: +Z is the part axis pointing *out of* the
//! surface it sits on, z = 0 is the seating plane (under-head plane for screws, top
//! for inserts/magnets, base for nuts/washers/standoffs/feet).

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use glam::DVec3;
use occt::{Edge, Frame, Shape};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LibraryError {
    #[error("unknown part `{0}`")]
    UnknownPart(String),
    #[error("part `{part}` has no size `{size}`")]
    UnknownSize { part: String, size: String },
    #[error("no hole rule `{choice:?}` for {thread}")]
    NoHoleRule { thread: String, choice: HoleChoice },
    #[error("part data: {0}")]
    Data(String),
    #[error(transparent)]
    Kernel(#[from] occt::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Units {
    Mm,
    In,
}

impl Units {
    pub fn to_mm(self, v: f64) -> f64 {
        match self {
            Units::Mm => v,
            Units::In => v * 25.4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum System {
    Metric,
    Imperial,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Screw,
    Nut,
    Washer,
    Standoff,
    Insert,
    Magnet,
    Foot,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Head {
    Socket,
    Button,
    Flat,
    Pan,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    Close,
    Normal,
    Loose,
}

/// Which hole a part implies in the body it is mounted to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HoleChoice {
    Clearance(Fit),
    /// Clearance hole plus counterbore for the part's head.
    Counterbore(Fit),
    /// Clearance hole plus countersink for the part's head.
    Countersink(Fit),
    /// Tap drill.
    Tap,
    /// Pilot hole for thread-forming screws in plastic.
    Pilot,
    /// Heat-set insert installation hole.
    Insert,
    /// Pocket matching the part outline (magnets, feet).
    Pocket,
}

/// Searchable part metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartInfo {
    pub source: String,
    pub id: String,
    pub name: String,
    pub category: Category,
    #[serde(default)]
    pub standard: Option<String>,
    #[serde(default)]
    pub system: Option<System>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub part_number: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// Data caveats (TODOs) to surface in the UI.
    #[serde(default)]
    pub notes: Vec<String>,
}

/// What a placed part implies for the body it is mounted to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mate {
    /// Thread designation used for hole rules (`M3`, `#4-40`).
    pub thread: Option<String>,
    pub head: Option<Head>,
    /// Length of the part below the seating plane (mm), e.g. screw shank.
    pub depth_below: f64,
    /// Pocket for magnets/feet (dia, depth) in mm.
    pub pocket: Option<(f64, f64)>,
}

/// A hole to cut, in mm. Axis runs from the surface (z = 0) into the material (-Z).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HoleSpec {
    pub diameter: f64,
    /// `None` = through all.
    pub depth: Option<f64>,
    pub counterbore: Option<(f64, f64)>,
    /// (diameter at surface, included angle in degrees)
    pub countersink: Option<(f64, f64)>,
    /// True if any value came from a rule of thumb rather than a table.
    pub derived: bool,
    pub description: String,
}

impl HoleSpec {
    /// Tool solid in the local frame (surface at z = 0, hole along -Z), extended
    /// 1 mm above the surface. `through` is used when `depth` is `None`.
    pub fn tool(&self, through: f64) -> Result<Shape, occt::Error> {
        let depth = self.depth.unwrap_or(through);
        let above = 1.0;
        let mut t = Shape::cylinder(DVec3::new(0.0, 0.0, -depth), DVec3::Z, self.diameter / 2.0, depth + above)?;
        if let Some((d, h)) = self.counterbore {
            t = t.fuse(&Shape::cylinder(DVec3::new(0.0, 0.0, -h), DVec3::Z, d / 2.0, h + above)?)?;
        }
        if let Some((d, angle)) = self.countersink {
            let h = (d - self.diameter) / 2.0 / (angle.to_radians() / 2.0).tan();
            if h > 1e-6 {
                t = t.fuse(&Shape::cone(DVec3::new(0.0, 0.0, -h), DVec3::Z, self.diameter / 2.0, d / 2.0, h)?)?;
                t = t.fuse(&Shape::cylinder(DVec3::ZERO, DVec3::Z, d / 2.0, above)?)?;
            }
        }
        Ok(t)
    }
}

/// A concrete, sized part ready to place.
#[derive(Clone, Debug)]
pub struct PartInstance {
    pub info: PartInfo,
    pub size: String,
    pub shape: Shape,
    pub mate: Mate,
}

impl PartInstance {
    /// The hole this part needs in the mounting body.
    pub fn hole(&self, choice: HoleChoice) -> Result<HoleSpec, LibraryError> {
        if choice == HoleChoice::Pocket {
            let (d, depth) = self.mate.pocket.ok_or(LibraryError::NoHoleRule { thread: self.size.clone(), choice })?;
            return Ok(HoleSpec {
                diameter: d,
                depth: Some(depth),
                counterbore: None,
                countersink: None,
                derived: true,
                description: format!("pocket ⌀{d:.2} × {depth:.2}"),
            });
        }
        let thread = self.mate.thread.clone().ok_or(LibraryError::NoHoleRule { thread: self.size.clone(), choice })?;
        let rules = hole_rules(&thread).ok_or(LibraryError::NoHoleRule { thread: thread.clone(), choice })?;
        let default_depth = (self.mate.depth_below > 0.0).then_some(self.mate.depth_below);
        rules.spec(choice, self.mate.head, default_depth)
    }
}

/// A provider of parts. Implemented by the built-in catalog and local STEP folders.
pub trait PartSource: Send + Sync {
    /// Stable id stored in documents (`builtin`, `local:<name>`).
    fn source_id(&self) -> &str;
    fn parts(&self) -> Vec<PartInfo>;
    fn sizes(&self, part_id: &str) -> Vec<String>;
    fn build(&self, part_id: &str, size: &str) -> Result<PartInstance, LibraryError>;
    /// Original STEP file for a part, if it comes from one (documents embed it as a blob).
    fn step_file(&self, _part_id: &str) -> Option<PathBuf> {
        None
    }
    /// Case-insensitive token search over name, id, standard, tags, vendor, part number.
    fn search(&self, query: &str) -> Vec<PartInfo> {
        search_infos(self.parts(), query)
    }
}

pub fn search_infos(parts: Vec<PartInfo>, query: &str) -> Vec<PartInfo> {
    let toks: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase()).collect();
    let mut scored: Vec<(usize, PartInfo)> = parts
        .into_iter()
        .filter_map(|p| {
            let hay = format!(
                "{} {} {} {} {} {} {:?}",
                p.name,
                p.id,
                p.standard.clone().unwrap_or_default(),
                p.tags.join(" "),
                p.vendor.clone().unwrap_or_default(),
                p.part_number.clone().unwrap_or_default(),
                p.category
            )
            .to_lowercase();
            let hits = toks.iter().filter(|t| hay.contains(t.as_str())).count();
            (toks.is_empty() || hits == toks.len()).then_some((hits, p))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
    scored.into_iter().map(|(_, p)| p).collect()
}

/// All configured part sources.
#[derive(Clone)]
pub struct Library {
    pub sources: Vec<Arc<dyn PartSource>>,
}

impl Default for Library {
    fn default() -> Self {
        Library { sources: vec![Arc::new(BuiltinSource)] }
    }
}

impl Library {
    pub fn add(&mut self, s: Arc<dyn PartSource>) {
        self.sources.retain(|o| o.source_id() != s.source_id());
        self.sources.push(s);
    }
    pub fn source(&self, id: &str) -> Option<&Arc<dyn PartSource>> {
        self.sources.iter().find(|s| s.source_id() == id)
    }
    pub fn search(&self, query: &str) -> Vec<PartInfo> {
        search_infos(self.sources.iter().flat_map(|s| s.parts()).collect(), query)
    }
    pub fn build(&self, source: &str, part: &str, size: &str) -> Result<PartInstance, LibraryError> {
        self.source(source).ok_or_else(|| LibraryError::UnknownPart(format!("{source}:{part}")))?.build(part, size)
    }
}

// ---------------------------------------------------------------------------
// Data tables

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Generator {
    Screw,
    HexNut,
    SquareNut,
    Washer,
    SplitWasher,
    Standoff,
    Insert,
    Magnet,
    Foot,
}

#[derive(Clone, Debug, Deserialize)]
struct Row {
    size: String,
    #[serde(default)]
    thread: Option<String>,
    #[serde(default)]
    d: Option<f64>,
    #[serde(default)]
    dk: Option<f64>,
    #[serde(default)]
    k: Option<f64>,
    #[serde(default)]
    s: Option<f64>,
    #[serde(default)]
    m: Option<f64>,
    #[serde(default)]
    d1: Option<f64>,
    #[serde(default)]
    d2: Option<f64>,
    #[serde(default)]
    h: Option<f64>,
    #[serde(default)]
    od: Option<f64>,
    #[serde(default)]
    l: Option<f64>,
    #[serde(default)]
    hole_d: Option<f64>,
    #[serde(default)]
    hole_depth: Option<f64>,
    #[serde(default)]
    male_l: Option<f64>,
    #[serde(default)]
    lengths: Vec<f64>,
    #[serde(default)]
    note: Option<String>,
}

impl Row {
    fn thread(&self) -> &str {
        self.thread.as_deref().unwrap_or(&self.size)
    }
}

#[derive(Clone, Debug, Deserialize)]
struct Table {
    id: String,
    name: String,
    standard: String,
    source: String,
    units: Units,
    system: System,
    generator: Generator,
    #[serde(default)]
    head: Option<Head>,
    #[serde(default)]
    head_angle: Option<f64>,
    #[serde(default)]
    variant: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    todo: Option<String>,
    sizes: Vec<Row>,
}

#[derive(Clone, Debug, Deserialize)]
struct BoreRow {
    d: f64,
    depth: f64,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct HoleRow {
    close: f64,
    normal: f64,
    #[serde(default)]
    loose: Option<f64>,
    tap_drill: f64,
    #[serde(default)]
    counterbore: std::collections::BTreeMap<String, BoreRow>,
}

#[derive(Clone, Debug, Deserialize)]
struct Derived {
    counterbore_dia_add: f64,
    counterbore_depth_add: f64,
    countersink_dia_add: f64,
}

#[derive(Clone, Debug, Deserialize)]
struct HoleFile {
    system: System,
    units: Units,
    source: String,
    pilot_factor_plastic: f64,
    sizes: std::collections::BTreeMap<String, HoleRow>,
    derived: Derived,
}

const TABLE_SRC: &[&str] = &[
    include_str!("../data/parts/iso4762.json"),
    include_str!("../data/parts/iso7380.json"),
    include_str!("../data/parts/iso10642.json"),
    include_str!("../data/parts/thread_forming_plastic.json"),
    include_str!("../data/parts/iso4026.json"),
    include_str!("../data/parts/iso4032.json"),
    include_str!("../data/parts/iso4035.json"),
    include_str!("../data/parts/din557.json"),
    include_str!("../data/parts/din562.json"),
    include_str!("../data/parts/iso7089.json"),
    include_str!("../data/parts/iso7092.json"),
    include_str!("../data/parts/din127b.json"),
    include_str!("../data/parts/standoff_metric_ff.json"),
    include_str!("../data/parts/standoff_metric_mf.json"),
    include_str!("../data/parts/insert_metric.json"),
    include_str!("../data/parts/misc_magnets_feet.json"),
    include_str!("../data/parts/misc_feet.json"),
    include_str!("../data/parts/asme_shcs.json"),
    include_str!("../data/parts/asme_button.json"),
    include_str!("../data/parts/asme_flat.json"),
    include_str!("../data/parts/asme_hex_nut.json"),
    include_str!("../data/parts/sae_washer.json"),
    include_str!("../data/parts/uss_washer.json"),
    include_str!("../data/parts/asme_split_lock.json"),
    include_str!("../data/parts/standoff_inch_ff.json"),
    include_str!("../data/parts/standoff_inch_mf.json"),
    include_str!("../data/parts/insert_inch.json"),
];

const HOLE_SRC: &[&str] = &[include_str!("../data/parts/holes_metric.json"), include_str!("../data/parts/holes_imperial.json")];

struct Catalog {
    tables: Vec<Table>,
    holes: Vec<HoleFile>,
}

fn catalog() -> &'static Catalog {
    static C: OnceLock<Catalog> = OnceLock::new();
    C.get_or_init(|| Catalog {
        tables: TABLE_SRC.iter().map(|s| serde_json::from_str(s).expect("built-in part table is valid JSON")).collect(),
        holes: HOLE_SRC.iter().map(|s| serde_json::from_str(s).expect("built-in hole table is valid JSON")).collect(),
    })
}

fn category(g: Generator) -> Category {
    match g {
        Generator::Screw => Category::Screw,
        Generator::HexNut | Generator::SquareNut => Category::Nut,
        Generator::Washer | Generator::SplitWasher => Category::Washer,
        Generator::Standoff => Category::Standoff,
        Generator::Insert => Category::Insert,
        Generator::Magnet => Category::Magnet,
        Generator::Foot => Category::Foot,
    }
}

// ---------------------------------------------------------------------------
// Sizes and lengths

/// Format inches as a fraction (`0.5` → `1/2`, `1.25` → `1-1/4`); denominators up to 64.
pub fn inch_fraction(v: f64) -> String {
    let n64 = (v * 64.0).round() as i64;
    let whole = n64 / 64;
    let mut num = n64 % 64;
    let mut den = 64;
    if num == 0 {
        return whole.to_string();
    }
    while num % 2 == 0 {
        num /= 2;
        den /= 2;
    }
    if whole > 0 {
        format!("{whole}-{num}/{den}")
    } else {
        format!("{num}/{den}")
    }
}

/// Parse `1/2`, `1-1/4`, `0.75`, `3`.
pub fn parse_inch(s: &str) -> Option<f64> {
    let s = s.trim();
    let frac = |f: &str| -> Option<f64> {
        let (a, b) = f.split_once('/')?;
        Some(a.trim().parse::<f64>().ok()? / b.trim().parse::<f64>().ok()?)
    };
    if let Some((w, f)) = s.split_once('-') {
        return Some(w.parse::<f64>().ok()? + frac(f)?);
    }
    if s.contains('/') {
        return frac(s);
    }
    s.parse().ok()
}

fn fmt_len(units: Units, l: f64) -> String {
    match units {
        Units::Mm => {
            if (l - l.round()).abs() < 1e-9 {
                format!("{}", l.round() as i64)
            } else {
                format!("{l}")
            }
        }
        Units::In => inch_fraction(l),
    }
}

impl Table {
    fn info(&self) -> PartInfo {
        let mut notes: Vec<String> = self.todo.iter().cloned().collect();
        notes.extend(self.sizes.iter().filter_map(|r| r.note.as_ref().map(|n| format!("{}: {n}", r.size))));
        PartInfo {
            source: BUILTIN.into(),
            id: self.id.clone(),
            name: self.name.clone(),
            category: category(self.generator),
            standard: Some(self.standard.clone()),
            system: Some(self.system),
            tags: self.tags.clone(),
            vendor: None,
            part_number: None,
            url: None,
            notes,
        }
    }

    fn sizes(&self) -> Vec<String> {
        let mut v = Vec::new();
        for r in &self.sizes {
            if r.lengths.is_empty() {
                v.push(r.size.clone());
            } else {
                v.extend(r.lengths.iter().map(|l| format!("{}x{}", r.size, fmt_len(self.units, *l))));
            }
        }
        v
    }

    /// Resolve a size string to (row, length in native units).
    fn row(&self, size: &str) -> Option<(&Row, Option<f64>)> {
        if let Some(r) = self.sizes.iter().find(|r| r.size == size && r.lengths.is_empty()) {
            return Some((r, None));
        }
        let (base, len) = size.rsplit_once(['x', 'X'])?;
        let r = self.sizes.iter().find(|r| r.size == base.trim() && !r.lengths.is_empty())?;
        let l = match self.units {
            Units::Mm => len.trim().parse().ok()?,
            Units::In => parse_inch(len)?,
        };
        Some((r, Some(l)))
    }
}

// ---------------------------------------------------------------------------
// Hole rules

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Bore {
    pub d: f64,
    pub depth: f64,
    pub derived: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Sink {
    pub d: f64,
    pub angle: f64,
    pub derived: bool,
}

/// Hole rules for one thread size, in the table's native units.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HoleRules {
    pub thread: String,
    pub system: System,
    pub units: Units,
    pub close: f64,
    pub normal: f64,
    pub loose: Option<f64>,
    pub tap_drill: f64,
    pub pilot_plastic: f64,
    pub counterbore: Vec<(Head, Bore)>,
    pub countersink: Vec<(Head, Sink)>,
    /// Heat-set insert installation hole (diameter, depth), from the insert table.
    pub insert: Option<(f64, f64)>,
    pub source: String,
}

/// Look up hole rules for a thread (`M3`, `#4-40`).
pub fn hole_rules(thread: &str) -> Option<HoleRules> {
    let cat = catalog();
    let (file, row) = cat.holes.iter().find_map(|f| f.sizes.get(thread).map(|r| (f, r)))?;
    let units = file.units;
    let d_nom = cat
        .tables
        .iter()
        .flat_map(|t| t.sizes.iter())
        .find(|r| r.thread() == thread)
        .and_then(|r| r.d)?;
    let mut counterbore = Vec::new();
    let mut countersink = Vec::new();
    for t in cat.tables.iter().filter(|t| t.generator == Generator::Screw && t.units == units) {
        let Some(head) = t.head else { continue };
        let Some(r) = t.sizes.iter().find(|r| r.thread() == thread) else { continue };
        match head {
            Head::Flat => {
                if let Some(dk) = r.dk {
                    if !countersink.iter().any(|(h, _)| *h == head) {
                        countersink.push((
                            head,
                            Sink { d: dk + file.derived.countersink_dia_add, angle: t.head_angle.unwrap_or(90.0), derived: true },
                        ));
                    }
                }
            }
            Head::Socket | Head::Button | Head::Pan => {
                if counterbore.iter().any(|(h, _)| *h == head) {
                    continue;
                }
                let key = format!("{head:?}").to_lowercase();
                if let Some(b) = row.counterbore.get(&key) {
                    counterbore.push((head, Bore { d: b.d, depth: b.depth, derived: b.note.is_some() }));
                } else if let (Some(dk), Some(k)) = (r.dk, r.k) {
                    counterbore.push((
                        head,
                        Bore { d: dk + file.derived.counterbore_dia_add, depth: k + file.derived.counterbore_depth_add, derived: true },
                    ));
                }
            }
            Head::None => {}
        }
    }
    let insert = cat
        .tables
        .iter()
        .filter(|t| t.generator == Generator::Insert && t.units == units)
        .flat_map(|t| t.sizes.iter())
        .find(|r| r.thread() == thread)
        .and_then(|r| Some((r.hole_d?, r.hole_depth?)));
    Some(HoleRules {
        thread: thread.into(),
        system: file.system,
        units,
        close: row.close,
        normal: row.normal,
        loose: row.loose,
        tap_drill: row.tap_drill,
        pilot_plastic: d_nom * file.pilot_factor_plastic,
        counterbore,
        countersink,
        insert,
        source: file.source.clone(),
    })
}

impl HoleRules {
    pub fn mm(&self, v: f64) -> f64 {
        self.units.to_mm(v)
    }

    /// Clearance diameter (native units) for a fit; `Loose` falls back to `None`
    /// when the table has no loose class.
    pub fn clearance(&self, fit: Fit) -> Option<f64> {
        match fit {
            Fit::Close => Some(self.close),
            Fit::Normal => Some(self.normal),
            Fit::Loose => self.loose,
        }
    }

    /// A hole specification in mm. `depth` (mm) is used for blind holes (tap, pilot).
    pub fn spec(&self, choice: HoleChoice, head: Option<Head>, depth: Option<f64>) -> Result<HoleSpec, LibraryError> {
        let no = || LibraryError::NoHoleRule { thread: self.thread.clone(), choice };
        let clear = |fit: Fit| self.clearance(fit).map(|d| self.mm(d)).ok_or_else(no);
        let head = head.unwrap_or(Head::Socket);
        Ok(match choice {
            HoleChoice::Clearance(fit) => HoleSpec {
                diameter: clear(fit)?,
                depth: None,
                counterbore: None,
                countersink: None,
                derived: false,
                description: format!("{} clearance ({fit:?})", self.thread),
            },
            HoleChoice::Counterbore(fit) => {
                let b = self.counterbore.iter().find(|(h, _)| *h == head).or(self.counterbore.first()).ok_or_else(no)?;
                HoleSpec {
                    diameter: clear(fit)?,
                    depth: None,
                    counterbore: Some((self.mm(b.1.d), self.mm(b.1.depth))),
                    countersink: None,
                    derived: b.1.derived,
                    description: format!("{} counterbore ({fit:?})", self.thread),
                }
            }
            HoleChoice::Countersink(fit) => {
                let s = self.countersink.iter().find(|(h, _)| *h == head).or(self.countersink.first()).ok_or_else(no)?;
                HoleSpec {
                    diameter: clear(fit)?,
                    depth: None,
                    counterbore: None,
                    countersink: Some((self.mm(s.1.d), s.1.angle)),
                    derived: s.1.derived,
                    description: format!("{} countersink {}° ({fit:?})", self.thread, s.1.angle),
                }
            }
            HoleChoice::Tap => HoleSpec {
                diameter: self.mm(self.tap_drill),
                depth,
                counterbore: None,
                countersink: None,
                derived: false,
                description: format!("{} tap drill", self.thread),
            },
            HoleChoice::Pilot => HoleSpec {
                diameter: self.mm(self.pilot_plastic),
                depth,
                counterbore: None,
                countersink: None,
                derived: true,
                description: format!("{} pilot for thread-forming into plastic", self.thread),
            },
            HoleChoice::Insert => {
                let (d, dep) = self.insert.ok_or_else(no)?;
                HoleSpec {
                    diameter: self.mm(d),
                    depth: Some(self.mm(dep)),
                    counterbore: None,
                    countersink: None,
                    derived: false,
                    description: format!("{} heat-set insert hole", self.thread),
                }
            }
            HoleChoice::Pocket => return Err(no()),
        })
    }
}

// ---------------------------------------------------------------------------
// Geometry

fn polygon_prism(pts: &[DVec3], h: f64) -> Result<Shape, occt::Error> {
    let edges: Vec<Edge> = (0..pts.len()).map(|i| Edge::Line(pts[i], pts[(i + 1) % pts.len()])).collect();
    Shape::face(&Frame::XY, &edges, &[])?.extrude(DVec3::new(0.0, 0.0, h))
}

/// Hexagonal prism (across flats `s`) from z0 to z0 + h.
fn hex_prism(s: f64, z0: f64, h: f64) -> Result<Shape, occt::Error> {
    let r = s / 3f64.sqrt();
    let pts: Vec<DVec3> = (0..6)
        .map(|i| {
            let a = std::f64::consts::FRAC_PI_3 * i as f64;
            DVec3::new(r * a.cos(), r * a.sin(), 0.0)
        })
        .collect();
    polygon_prism(&pts, h)?.translated(DVec3::new(0.0, 0.0, z0))
}

fn cyl(r: f64, z0: f64, z1: f64) -> Result<Shape, occt::Error> {
    Shape::cylinder(DVec3::new(0.0, 0.0, z0), DVec3::Z, r, z1 - z0)
}

/// Spherical-cap button head of diameter `dk` and height `k` on z = 0.
fn dome(dk: f64, k: f64) -> Result<Shape, occt::Error> {
    let a = dk / 2.0;
    let r = (a * a + k * k) / (2.0 * k);
    let c = DVec3::new(0.0, 0.0, k - r);
    let start = DVec3::new(a, 0.0, 0.0);
    let top = DVec3::new(0.0, 0.0, k);
    let a0 = (start - c).z.atan2((start - c).x);
    let mid_ang = (a0 + std::f64::consts::FRAC_PI_2) / 2.0;
    let mid = c + DVec3::new(mid_ang.cos(), 0.0, mid_ang.sin()) * r;
    let frame = Frame { origin: DVec3::ZERO, x_dir: DVec3::X, normal: DVec3::NEG_Y };
    let f = Shape::face(&frame, &[Edge::Line(DVec3::ZERO, start), Edge::Arc3(start, mid, top), Edge::Line(top, DVec3::ZERO)], &[])?;
    f.revolve(DVec3::ZERO, DVec3::Z, 360.0)
}

fn build_shape(t: &Table, row: &Row, len: Option<f64>) -> Result<(Shape, Mate), LibraryError> {
    let u = |v: Option<f64>, what: &str| -> Result<f64, LibraryError> {
        v.map(|x| t.units.to_mm(x)).ok_or_else(|| LibraryError::Data(format!("{} {}: missing `{what}`", t.id, row.size)))
    };
    let thread = Some(row.thread().to_string());
    let mut mate = Mate { thread, head: t.head, depth_below: 0.0, pocket: None };
    let shape = match t.generator {
        Generator::Screw => {
            let d = u(row.d, "d")?;
            let l = u(len, "length")?;
            let head = t.head.unwrap_or(Head::None);
            let socket = |s: f64, top: f64, depth: f64| hex_prism(s, top - depth, depth + 1.0);
            match head {
                Head::Socket | Head::Button | Head::Pan => {
                    let (dk, k) = (u(row.dk, "dk")?, u(row.k, "k")?);
                    let mut h = match head {
                        Head::Button => dome(dk, k)?,
                        _ => cyl(dk / 2.0, 0.0, k)?,
                    };
                    if let Some(s) = row.s {
                        h = h.cut(&socket(t.units.to_mm(s), k, 0.6 * k)?)?;
                    }
                    mate.depth_below = l;
                    h.fuse(&cyl(d / 2.0, -l, 0.0)?)?.unify()?
                }
                Head::Flat => {
                    let dk = u(row.dk, "dk")?;
                    let angle = t.head_angle.unwrap_or(90.0);
                    let hk = (dk - d) / 2.0 / (angle.to_radians() / 2.0).tan();
                    let mut h = Shape::cone(DVec3::new(0.0, 0.0, -hk), DVec3::Z, d / 2.0, dk / 2.0, hk)?;
                    if let Some(s) = row.s {
                        h = h.cut(&socket(t.units.to_mm(s), 0.0, 0.7 * hk)?)?;
                    }
                    mate.depth_below = l;
                    h.fuse(&cyl(d / 2.0, -l, -hk)?)?.unify()?
                }
                Head::None => {
                    let s = u(row.s, "s")?;
                    mate.depth_below = l;
                    cyl(d / 2.0, -l, 0.0)?.cut(&socket(s, 0.0, (0.5 * l).min(d))?)?
                }
            }
        }
        Generator::HexNut => {
            let (d, s, m) = (u(row.d, "d")?, u(row.s, "s")?, u(row.m, "m")?);
            hex_prism(s, 0.0, m)?.cut(&cyl(d / 2.0, -1.0, m + 1.0)?)?
        }
        Generator::SquareNut => {
            let (d, s, m) = (u(row.d, "d")?, u(row.s, "s")?, u(row.m, "m")?);
            Shape::make_box(DVec3::new(-s / 2.0, -s / 2.0, 0.0), DVec3::new(s, s, m))?.cut(&cyl(d / 2.0, -1.0, m + 1.0)?)?
        }
        Generator::Washer | Generator::SplitWasher => {
            let (d1, d2, h) = (u(row.d1, "d1")?, u(row.d2, "d2")?, u(row.h, "h")?);
            let mut w = cyl(d2 / 2.0, 0.0, h)?.cut(&cyl(d1 / 2.0, -1.0, h + 1.0)?)?;
            if t.generator == Generator::SplitWasher {
                let gap = h.max(0.3);
                w = w.cut(&Shape::make_box(DVec3::new(0.0, -gap / 2.0, -1.0), DVec3::new(d2, gap, h + 2.0))?)?;
            }
            w
        }
        Generator::Standoff => {
            let (d, s) = (u(row.d, "d")?, u(row.s, "s")?);
            let l = u(len, "length")?;
            // cosmetic thread: holes at ~tap-drill size
            let minor = d * 0.82;
            let mut so = hex_prism(s, 0.0, l)?;
            if t.variant.as_deref() == Some("mf") {
                let ml = u(row.male_l, "male_l")?;
                so = so.cut(&cyl(minor / 2.0, (l - 1.5 * d).max(l * 0.4), l + 1.0)?)?;
                so = so.fuse(&cyl(d / 2.0, -ml, 0.0)?)?.unify()?;
                mate.depth_below = ml;
            } else {
                so = so.cut(&cyl(minor / 2.0, -1.0, l + 1.0)?)?;
            }
            so
        }
        Generator::Insert => {
            let (d, od, l) = (u(row.d, "d")?, u(row.od, "od")?, u(row.l, "l")?);
            mate.depth_below = l;
            cyl(od / 2.0, -l, 0.0)?.cut(&cyl(d / 2.0, -l - 1.0, 1.0)?)?
        }
        Generator::Magnet => {
            let (od, l) = (u(row.od, "od")?, u(row.l, "l")?);
            mate.pocket = Some((od + 0.2, l + 0.1));
            mate.thread = None;
            mate.depth_below = l;
            cyl(od / 2.0, -l, 0.0)?
        }
        Generator::Foot => {
            let (od, l) = (u(row.od, "od")?, u(row.l, "l")?);
            mate.pocket = Some((od + 0.5, 0.5));
            mate.thread = None;
            cyl(od / 2.0, 0.0, l)?
        }
    };
    Ok((shape, mate))
}

// ---------------------------------------------------------------------------
// Sources

pub const BUILTIN: &str = "builtin";

/// The built-in parametric catalog.
pub struct BuiltinSource;

impl BuiltinSource {
    fn table(&self, id: &str) -> Result<&'static Table, LibraryError> {
        catalog().tables.iter().find(|t| t.id == id).ok_or_else(|| LibraryError::UnknownPart(id.into()))
    }

    /// Provenance text for a table (standard + source citation).
    pub fn citation(&self, id: &str) -> Option<String> {
        self.table(id).ok().map(|t| format!("{} — {}", t.standard, t.source))
    }
}

impl PartSource for BuiltinSource {
    fn source_id(&self) -> &str {
        BUILTIN
    }
    fn parts(&self) -> Vec<PartInfo> {
        catalog().tables.iter().map(|t| t.info()).collect()
    }
    fn sizes(&self, part_id: &str) -> Vec<String> {
        self.table(part_id).map(|t| t.sizes()).unwrap_or_default()
    }
    fn build(&self, part_id: &str, size: &str) -> Result<PartInstance, LibraryError> {
        let t = self.table(part_id)?;
        let (row, len) = t.row(size).ok_or_else(|| LibraryError::UnknownSize { part: part_id.into(), size: size.into() })?;
        let (shape, mate) = build_shape(t, row, len)?;
        Ok(PartInstance { info: t.info(), size: size.into(), shape, mate })
    }
}

/// One entry of a local library's `library.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LocalEntry {
    pub id: String,
    /// STEP file relative to the library folder.
    pub file: String,
    pub name: String,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub part_number: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default = "default_cat")]
    pub category: Category,
    /// Thread for hole rules (e.g. a vendor insert that takes `M3`).
    #[serde(default)]
    pub thread: Option<String>,
    #[serde(default)]
    pub head: Option<Head>,
    #[serde(default)]
    pub depth_below: f64,
}

fn default_cat() -> Category {
    Category::Other
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LocalLibraryFile {
    pub parts: Vec<LocalEntry>,
}

/// A folder of STEP files with a `library.json` sidecar of metadata.
pub struct LocalStepSource {
    id: String,
    pub dir: PathBuf,
    pub entries: Vec<LocalEntry>,
}

impl LocalStepSource {
    pub fn open(name: &str, dir: &Path) -> Result<LocalStepSource, LibraryError> {
        let meta = dir.join("library.json");
        let txt = std::fs::read_to_string(&meta).map_err(|e| LibraryError::Data(format!("{}: {e}", meta.display())))?;
        let f: LocalLibraryFile = serde_json::from_str(&txt).map_err(|e| LibraryError::Data(format!("{}: {e}", meta.display())))?;
        Ok(LocalStepSource { id: format!("local:{name}"), dir: dir.to_path_buf(), entries: f.parts })
    }

    fn entry(&self, id: &str) -> Result<&LocalEntry, LibraryError> {
        self.entries.iter().find(|e| e.id == id).ok_or_else(|| LibraryError::UnknownPart(id.into()))
    }

    pub fn entry_info(&self, e: &LocalEntry) -> PartInfo {
        PartInfo {
            source: self.id.clone(),
            id: e.id.clone(),
            name: e.name.clone(),
            category: e.category,
            standard: None,
            system: None,
            tags: e.tags.clone(),
            vendor: e.vendor.clone(),
            part_number: e.part_number.clone(),
            url: e.url.clone(),
            notes: Vec::new(),
        }
    }
}

/// Mate metadata for a local entry (shared with documents that embed the STEP).
pub fn local_mate(e: &LocalEntry) -> Mate {
    Mate { thread: e.thread.clone(), head: e.head, depth_below: e.depth_below, pocket: None }
}

impl PartSource for LocalStepSource {
    fn source_id(&self) -> &str {
        &self.id
    }
    fn parts(&self) -> Vec<PartInfo> {
        self.entries.iter().map(|e| self.entry_info(e)).collect()
    }
    fn sizes(&self, part_id: &str) -> Vec<String> {
        if self.entry(part_id).is_ok() {
            vec!["-".into()]
        } else {
            Vec::new()
        }
    }
    fn build(&self, part_id: &str, size: &str) -> Result<PartInstance, LibraryError> {
        let e = self.entry(part_id)?;
        let shape = Shape::read_step(&self.dir.join(&e.file))?;
        Ok(PartInstance { info: self.entry_info(e), size: size.into(), shape, mate: local_mate(e) })
    }
    fn step_file(&self, part_id: &str) -> Option<PathBuf> {
        self.entry(part_id).ok().map(|e| self.dir.join(&e.file))
    }
}

/// Rigidly place a local-frame shape: +Z → `axis`, then spin by `spin_deg`, then move to `origin`.
pub fn place(shape: &Shape, origin: DVec3, axis: DVec3, spin_deg: f64) -> Result<Shape, occt::Error> {
    let a = axis.normalize_or_zero();
    let mut s = if spin_deg.abs() > 1e-12 { shape.rotated(DVec3::ZERO, DVec3::Z, spin_deg)? } else { shape.clone() };
    let dot = a.dot(DVec3::Z);
    if dot < -1.0 + 1e-12 {
        s = s.rotated(DVec3::ZERO, DVec3::X, 180.0)?;
    } else if dot < 1.0 - 1e-12 {
        let ax = DVec3::Z.cross(a);
        s = s.rotated(DVec3::ZERO, ax, dot.clamp(-1.0, 1.0).acos().to_degrees())?;
    }
    if origin != DVec3::ZERO {
        s = s.translated(origin)?;
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractions() {
        assert_eq!(inch_fraction(0.5), "1/2");
        assert_eq!(inch_fraction(1.25), "1-1/4");
        assert_eq!(inch_fraction(0.1875), "3/16");
        assert_eq!(inch_fraction(2.0), "2");
        assert_eq!(parse_inch("1-1/4"), Some(1.25));
        assert_eq!(parse_inch("3/8"), Some(0.375));
    }

    #[test]
    fn all_tables_parse_and_cite_sources() {
        let c = catalog();
        assert!(c.tables.len() >= 27);
        for t in &c.tables {
            assert!(!t.source.is_empty() && !t.standard.is_empty(), "{}", t.id);
            assert!(!t.sizes.is_empty(), "{}", t.id);
        }
    }

    #[test]
    fn hole_rules_m3() {
        let r = hole_rules("M3").unwrap();
        assert_eq!((r.close, r.normal, r.loose), (3.2, 3.4, Some(3.6)));
        assert_eq!(r.tap_drill, 2.5);
        let cb = &r.counterbore.iter().find(|(h, _)| *h == Head::Socket).unwrap().1;
        assert_eq!((cb.d, cb.depth, cb.derived), (6.5, 3.3, false));
        assert_eq!(r.insert, Some((4.0, 6.7)));
        assert!((r.pilot_plastic - 2.4).abs() < 1e-12);
        let spec = r.spec(HoleChoice::Counterbore(Fit::Normal), Some(Head::Socket), None).unwrap();
        assert_eq!((spec.diameter, spec.counterbore), (3.4, Some((6.5, 3.3))));
        let cs = r.spec(HoleChoice::Countersink(Fit::Normal), Some(Head::Flat), None).unwrap();
        assert_eq!(cs.countersink.unwrap().1, 90.0);
        assert!(cs.derived);
    }

    #[test]
    fn hole_rules_4_40() {
        let r = hole_rules("#4-40").unwrap();
        assert_eq!(r.units, Units::In);
        assert_eq!((r.close, r.normal, r.loose), (0.1160, 0.1285, None));
        assert_eq!(r.tap_drill, 0.0890);
        let spec = r.spec(HoleChoice::Clearance(Fit::Close), None, None).unwrap();
        assert!((spec.diameter - 2.9464).abs() < 1e-9);
        let spec = r.spec(HoleChoice::Counterbore(Fit::Normal), Some(Head::Socket), None).unwrap();
        let (d, depth) = spec.counterbore.unwrap();
        assert!((d - 0.2188 * 25.4).abs() < 1e-9 && (depth - 0.112 * 25.4).abs() < 1e-9);
        assert!(r.spec(HoleChoice::Clearance(Fit::Loose), None, None).is_err());
        let cs = r.spec(HoleChoice::Countersink(Fit::Normal), Some(Head::Flat), None).unwrap();
        assert_eq!(cs.countersink.unwrap().1, 82.0);
    }

    #[test]
    fn sizes_and_search() {
        let b = BuiltinSource;
        let s = b.sizes("iso4762");
        assert!(s.contains(&"M3x10".to_string()));
        let s = b.sizes("asme_shcs");
        assert!(s.contains(&"#4-40x1/2".to_string()) && s.contains(&"1/4-20x1-1/4".to_string()));
        let hits = Library::default().search("cap screw inch");
        assert_eq!(hits[0].id, "asme_shcs");
        assert!(Library::default().search("heat set").iter().any(|p| p.id == "insert_m"));
    }
}
