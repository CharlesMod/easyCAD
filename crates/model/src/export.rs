//! Exports: binary STL and 3MF (written in Rust from the display mesh, with
//! vertices welded so slicers see a manifold mesh), STEP via the kernel.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use occt::{Mesh, Shape};

use crate::regen::Body;

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error(transparent)]
    Kernel(#[from] occt::Error),
    #[error("mesh: {0}")]
    Mesh(String),
    #[error("nothing to export")]
    Empty,
}

/// Print-quality tessellation for STL/3MF (finer than the display mesh).
pub const PRINT_LIN_DEFL: f64 = 0.02;
pub const PRINT_ANG_DEFL_DEG: f64 = 8.0;

/// An indexed triangle mesh with welded vertices.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IndexedMesh {
    pub vertices: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
}

/// Weld identical (quantised to 1e-5 mm) positions; drop degenerate triangles.
pub fn weld(m: &Mesh) -> IndexedMesh {
    let mut map: HashMap<[i64; 3], u32> = HashMap::new();
    let mut out = IndexedMesh::default();
    let q = |v: f32| (v as f64 * 1e5).round() as i64;
    let remap: Vec<u32> = m
        .positions
        .iter()
        .map(|p| {
            *map.entry([q(p[0]), q(p[1]), q(p[2])]).or_insert_with(|| {
                out.vertices.push(*p);
                out.vertices.len() as u32 - 1
            })
        })
        .collect();
    for t in &m.indices {
        let t = [remap[t[0] as usize], remap[t[1] as usize], remap[t[2] as usize]];
        if t[0] != t[1] && t[1] != t[2] && t[0] != t[2] {
            out.triangles.push(t);
        }
    }
    out
}

fn print_mesh(bodies: &[&Body]) -> Result<Vec<IndexedMesh>, ExportError> {
    if bodies.is_empty() {
        return Err(ExportError::Empty);
    }
    bodies.iter().map(|b| Ok(weld(&b.shape().tessellate(PRINT_LIN_DEFL, PRINT_ANG_DEFL_DEG)?))).collect()
}

fn normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-30);
    [n[0] / l, n[1] / l, n[2] / l]
}

/// Binary STL of all bodies in one file.
pub fn write_stl(bodies: &[&Body], path: &Path) -> Result<(), ExportError> {
    let meshes = print_mesh(bodies)?;
    let n: usize = meshes.iter().map(|m| m.triangles.len()).sum();
    let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut header = [0u8; 80];
    let tag = b"easyCAD STL mm";
    header[..tag.len()].copy_from_slice(tag);
    w.write_all(&header)?;
    w.write_all(&(n as u32).to_le_bytes())?;
    for m in &meshes {
        for t in &m.triangles {
            let (a, b, c) = (m.vertices[t[0] as usize], m.vertices[t[1] as usize], m.vertices[t[2] as usize]);
            for v in [normal(a, b, c), a, b, c] {
                for x in v {
                    w.write_all(&x.to_le_bytes())?;
                }
            }
            w.write_all(&[0, 0])?;
        }
    }
    w.flush()?;
    Ok(())
}

/// 3MF package: one `<object>` per body, all placed in the build, units mm.
pub fn write_3mf(bodies: &[&Body], path: &Path) -> Result<(), ExportError> {
    let meshes = print_mesh(bodies)?;
    let mut model = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<model unit=\"millimeter\" xml:lang=\"en-US\" xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\">\n <metadata name=\"Application\">easyCAD</metadata>\n <resources>\n",
    );
    for (i, (m, b)) in meshes.iter().zip(bodies).enumerate() {
        model += &format!("  <object id=\"{}\" type=\"model\" name=\"{}\">\n   <mesh>\n    <vertices>\n", i + 1, xml_escape(&b.name));
        for v in &m.vertices {
            model += &format!("     <vertex x=\"{}\" y=\"{}\" z=\"{}\"/>\n", v[0], v[1], v[2]);
        }
        model += "    </vertices>\n    <triangles>\n";
        for t in &m.triangles {
            model += &format!("     <triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"/>\n", t[0], t[1], t[2]);
        }
        model += "    </triangles>\n   </mesh>\n  </object>\n";
    }
    model += " </resources>\n <build>\n";
    for i in 0..meshes.len() {
        model += &format!("  <item objectid=\"{}\"/>\n", i + 1);
    }
    model += " </build>\n</model>\n";

    let f = std::fs::File::create(path)?;
    let mut z = zip::ZipWriter::new(f);
    let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    z.start_file("[Content_Types].xml", opt)?;
    z.write_all(
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\n <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\n <Default Extension=\"model\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"/>\n</Types>\n",
    )?;
    z.start_file("_rels/.rels", opt)?;
    z.write_all(
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n <Relationship Target=\"/3D/3dmodel.model\" Id=\"rel0\" Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\"/>\n</Relationships>\n",
    )?;
    z.start_file("3D/3dmodel.model", opt)?;
    z.write_all(model.as_bytes())?;
    z.finish()?;
    Ok(())
}

/// 3MF of an assembly: one `<object>` per entry of `objects` (its bodies merged in
/// the object's own frame) and one build `<item>` per `(object index, transform)`
/// (row-major 3×4 rotation | translation, as `assembly::Transform::to_rows`).
pub fn write_3mf_assembly(objects: &[(String, Vec<&Body>)], items: &[(usize, [f64; 12])], path: &Path) -> Result<(), ExportError> {
    if objects.is_empty() || items.is_empty() {
        return Err(ExportError::Empty);
    }
    let mut model = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<model unit=\"millimeter\" xml:lang=\"en-US\" xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\">\n <metadata name=\"Application\">easyCAD</metadata>\n <resources>\n",
    );
    for (i, (name, bodies)) in objects.iter().enumerate() {
        model += &format!("  <object id=\"{}\" type=\"model\" name=\"{}\">\n   <mesh>\n    <vertices>\n", i + 1, xml_escape(name));
        let meshes = print_mesh(bodies)?;
        let mut base = 0u32;
        let mut tris = String::new();
        for m in &meshes {
            for v in &m.vertices {
                model += &format!("     <vertex x=\"{}\" y=\"{}\" z=\"{}\"/>\n", v[0], v[1], v[2]);
            }
            for t in &m.triangles {
                tris += &format!("     <triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"/>\n", t[0] + base, t[1] + base, t[2] + base);
            }
            base += m.vertices.len() as u32;
        }
        model += "    </vertices>\n    <triangles>\n";
        model += &tris;
        model += "    </triangles>\n   </mesh>\n  </object>\n";
    }
    model += " </resources>\n <build>\n";
    for (k, r) in items {
        // 3MF uses row vectors: p' = p · M, M = [m00 m01 m02; m10 ..; m20 ..; m30 m31 m32]
        let m = [r[0], r[4], r[8], r[1], r[5], r[9], r[2], r[6], r[10], r[3], r[7], r[11]];
        let t: Vec<String> = m.iter().map(|x| format!("{}", if x.abs() < 1e-15 { 0.0 } else { *x })).collect();
        model += &format!("  <item objectid=\"{}\" transform=\"{}\"/>\n", k + 1, t.join(" "));
    }
    model += " </build>\n</model>\n";
    write_3mf_package(&model, path)
}

fn write_3mf_package(model: &str, path: &Path) -> Result<(), ExportError> {
    let f = std::fs::File::create(path)?;
    let mut z = zip::ZipWriter::new(f);
    let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    z.start_file("[Content_Types].xml", opt)?;
    z.write_all(
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\n <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\n <Default Extension=\"model\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"/>\n</Types>\n",
    )?;
    z.start_file("_rels/.rels", opt)?;
    z.write_all(
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n <Relationship Target=\"/3D/3dmodel.model\" Id=\"rel0\" Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\"/>\n</Relationships>\n",
    )?;
    z.start_file("3D/3dmodel.model", opt)?;
    z.write_all(model.as_bytes())?;
    z.finish()?;
    Ok(())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// STEP (AP214) of the given bodies, solids and reference bodies alike.
pub fn write_step(bodies: &[&Body], path: &Path) -> Result<(), ExportError> {
    if bodies.is_empty() {
        return Err(ExportError::Empty);
    }
    // one STEP part per body (per sub-part for assemblies), keeping names/colours
    let mut shapes: Vec<Shape> = Vec::new();
    let mut parts: Vec<occt::StepPart> = Vec::new();
    // face colours of their own (imported face styles), in the compound's face order
    let mut face_colors: Vec<Option<[f32; 3]>> = Vec::new();
    for b in bodies {
        let own = |f: usize| b.appearance().and_then(|a| a.face_color.get(f).copied().flatten());
        match b.appearance().filter(|a| a.parts.len() > 1 && b.shape().kind() == Some(occt::ShapeKind::Compound)) {
            Some(a) => {
                let solids = b.shape().solids()?;
                for (k, s) in solids.into_iter().enumerate() {
                    let p = a.parts.get(k);
                    parts.push(occt::StepPart { name: p.map(|p| p.name.clone()).unwrap_or_else(|| b.name.clone()), color: p.and_then(|p| p.color) });
                    let mine: Vec<Option<[f32; 3]>> = a.face_part.iter().enumerate().filter(|(_, o)| **o as usize == k).map(|(f, _)| own(f)).collect();
                    let nf = s.count(occt::ShapeKind::Face) as usize;
                    face_colors.extend(if mine.len() == nf { mine } else { vec![None; nf] });
                    shapes.push(s);
                }
            }
            None => {
                parts.push(occt::StepPart { name: b.name.clone(), color: b.appearance().and_then(|a| a.parts.first()).and_then(|p| p.color) });
                face_colors.extend((0..b.shape().count(occt::ShapeKind::Face) as usize).map(own));
                shapes.push(b.shape().clone());
            }
        }
    }
    let refs: Vec<&Shape> = shapes.iter().collect();
    let c = Shape::compound(&refs)?;
    if face_colors.len() != c.count(occt::ShapeKind::Face) as usize {
        face_colors.clear(); // faces shared between bodies: keep part colours only
    }
    std::fs::write(path, c.to_step_string_styled(&parts, &face_colors)?)?;
    Ok(())
}

/// Kernel STL writer (alternative to [`write_stl`]).
pub fn write_stl_occt(body: &Body, path: &Path) -> Result<(), ExportError> {
    body.shape().write_stl(path, PRINT_LIN_DEFL, false)?;
    Ok(())
}

/// Count boundary edges (edges used by exactly one triangle); 0 means watertight.
pub fn open_edges(m: &IndexedMesh) -> usize {
    let mut count: HashMap<(u32, u32), i32> = HashMap::new();
    for t in &m.triangles {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            *count.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    count.values().filter(|c| **c == 1).count()
}
