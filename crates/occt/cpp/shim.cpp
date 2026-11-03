// Narrow OpenCascade shim for Casework. Every entry point either returns a valid
// result or throws (std::runtime_error / Standard_Failure); `shim.h` converts
// exceptions into Rust `cxx::Exception`s.

#include "shim.h"
#include "occt/src/ffi.rs.h"

#include <algorithm>
#include <cmath>
#include <cstdlib>
#include <mutex>
#include <sstream>
#include <stdexcept>
#include <vector>

#include <BRepAdaptor_Curve.hxx>
#include <HLRAlgo_Projector.hxx>
#include <HLRBRep_Algo.hxx>
#include <HLRBRep_HLRToShape.hxx>
#include <HLRBRep_PolyAlgo.hxx>
#include <HLRBRep_PolyHLRToShape.hxx>
#include <BRepAdaptor_Curve2d.hxx>
#include <BRepAdaptor_CompCurve.hxx>
#include <BRepClass_FaceClassifier.hxx>
#include <BRepOffsetAPI_MakeOffset.hxx>
#include <BRepOffsetAPI_MakeOffsetShape.hxx>
#include <ShapeAnalysis_Curve.hxx>
#include <TopoDS_Vertex.hxx>
#include <TopoDS_Solid.hxx>
#include <BRepAdaptor_Surface.hxx>
#include <BRepAlgoAPI_Common.hxx>
#include <BRepAlgoAPI_Cut.hxx>
#include <BRepAlgoAPI_Fuse.hxx>
#include <BRepBndLib.hxx>
#include <BRepBuilderAPI_MakeEdge.hxx>
#include <BRepBuilderAPI_MakeFace.hxx>
#include <BRepBuilderAPI_MakeWire.hxx>
#include <BRepBuilderAPI_Transform.hxx>
#include <BRepCheck_Analyzer.hxx>
#include <BRepFilletAPI_MakeChamfer.hxx>
#include <BRepFilletAPI_MakeFillet.hxx>
#include <BRepGProp.hxx>
#include <BRepLib_ToolTriangulatedShape.hxx>
#include <BRepMesh_IncrementalMesh.hxx>
#include <BRepOffsetAPI_DraftAngle.hxx>
#include <BRepOffsetAPI_MakeThickSolid.hxx>
#include <BRepPrimAPI_MakeBox.hxx>
#include <BRepPrimAPI_MakeCone.hxx>
#include <BRepPrimAPI_MakeCylinder.hxx>
#include <BRepPrimAPI_MakePrism.hxx>
#include <BRepPrimAPI_MakeRevol.hxx>
#include <BRepTools.hxx>
#include <BRep_Builder.hxx>
#include <BRep_Tool.hxx>
#include <Bnd_Box.hxx>
#include <GCPnts_AbscissaPoint.hxx>
#include <GCPnts_TangentialDeflection.hxx>
#include <GC_MakeArcOfCircle.hxx>
#include <GProp_GProps.hxx>
#include <GeomAPI_ProjectPointOnSurf.hxx>
#include <GeomLProp_SLProps.hxx>
#include <Geom_Surface.hxx>
#include <IFSelect_ReturnStatus.hxx>
#include <Interface_Static.hxx>
#include <Message.hxx>
#include <Message_Messenger.hxx>
#include <Poly_Triangulation.hxx>
#include <STEPControl_Reader.hxx>
#include <STEPControl_Writer.hxx>
#include <ShapeFix_Face.hxx>
#include <ShapeFix_Shape.hxx>
#include <ShapeFix_Wire.hxx>
#include <ShapeUpgrade_UnifySameDomain.hxx>
#include <StlAPI_Writer.hxx>
#include <TopExp.hxx>
#include <TopExp_Explorer.hxx>
#include <TopTools_IndexedDataMapOfShapeListOfShape.hxx>
#include <TopTools_IndexedMapOfShape.hxx>
#include <TopTools_ListOfShape.hxx>
#include <TopoDS.hxx>
#include <TopoDS_Compound.hxx>
#include <TopoDS_Edge.hxx>
#include <TopoDS_Face.hxx>
#include <TopoDS_Wire.hxx>
#include <gp_Ax1.hxx>
#include <gp_Ax2.hxx>
#include <gp_Ax3.hxx>
#include <gp_Circ.hxx>
#include <gp_Pln.hxx>
#include <gp_Trsf.hxx>

namespace cw {

namespace {

constexpr double kTol = 1e-7;

[[noreturn]] void fail(const std::string &msg) { throw std::runtime_error(msg); }

gp_Pnt P(const V3 &v) { return gp_Pnt(v.x, v.y, v.z); }
gp_Vec Vv(const V3 &v) { return gp_Vec(v.x, v.y, v.z); }
gp_Dir D(const V3 &v, const char *what) {
  double n = std::sqrt(v.x * v.x + v.y * v.y + v.z * v.z);
  if (n < 1e-12) fail(std::string("zero-length direction: ") + what);
  return gp_Dir(v.x / n, v.y / n, v.z / n);
}
V3 mk(double x, double y, double z) {
  V3 r;
  r.x = x;
  r.y = y;
  r.z = z;
  return r;
}
V3 toV(const gp_XYZ &p) { return mk(p.X(), p.Y(), p.Z()); }
V3 toV(const gp_Pnt &p) { return toV(p.XYZ()); }
V3 toV(const gp_Dir &p) { return toV(p.XYZ()); }

gp_Ax3 frameAx(const Frame &f) { return gp_Ax3(P(f.origin), D(f.normal, "frame normal"), D(f.x_dir, "frame x")); }

std::unique_ptr<Shape> wrap(const TopoDS_Shape &s) {
  if (s.IsNull()) fail("operation produced an empty shape");
  return std::make_unique<Shape>(s);
}

void requireShape(const Shape &s, const char *what) {
  if (s.s.IsNull()) fail(std::string(what) + ": input shape is null");
}

// Unwrap single-solid compounds so the result is "just a solid" where possible.
TopoDS_Shape simplify(const TopoDS_Shape &s) {
  if (s.IsNull() || s.ShapeType() != TopAbs_COMPOUND) return s;
  TopoDS_Shape only;
  int n = 0;
  for (TopoDS_Iterator it(s); it.More(); it.Next()) {
    only = it.Value();
    ++n;
  }
  if (n == 1) return simplify(only);
  return s;
}

void checkValid(const TopoDS_Shape &s, const char *what) {
  BRepCheck_Analyzer an(s);
  if (!an.IsValid()) {
    // Try a cheap heal before giving up.
    fail(std::string(what) + " produced an invalid shape");
  }
}

TopoDS_Shape healIfInvalid(const TopoDS_Shape &s, const char *what) {
  if (s.IsNull()) fail(std::string(what) + " produced no shape");
  BRepCheck_Analyzer an(s);
  if (an.IsValid()) return s;
  Handle(ShapeFix_Shape) fix = new ShapeFix_Shape(s);
  fix->Perform();
  TopoDS_Shape r = fix->Shape();
  if (r.IsNull()) fail(std::string(what) + " produced an invalid shape");
  checkValid(r, what);
  return r;
}

// A draft that shrinks a convex corner radius below zero yields a degenerate cone that
// BRepCheck accepts but booleans cannot handle. Reject it explicitly.
void checkDraftCones(const TopoDS_Shape &s, const char *what) {
  for (TopExp_Explorer ex(s, TopAbs_FACE); ex.More(); ex.Next()) {
    TopoDS_Face f = TopoDS::Face(ex.Current());
    BRepAdaptor_Surface as(f, Standard_True);
    if (as.GetType() != GeomAbs_Cone) continue;
    gp_Cone c = as.Cone();
    double u0, u1, v0, v1;
    BRepTools::UVBounds(f, u0, u1, v0, v1);
    for (double v : {v0, v1}) {
      double r = c.RefRadius() + v * std::sin(c.SemiAngle());
      if (r < -1e-7)
        fail(std::string(what) + ": draft angle too large for a convex corner radius (the radius would go negative over the height)");
    }
  }
}

TopTools_IndexedMapOfShape mapOf(const TopoDS_Shape &s, TopAbs_ShapeEnum kind) {
  TopTools_IndexedMapOfShape m;
  TopExp::MapShapes(s, kind, m);
  return m;
}

// Parallel booleans / meshing (set CW_SERIAL_BOP=1 to compare against serial).
bool parallelBop() {
  static const bool on = std::getenv("CW_SERIAL_BOP") == nullptr;
  return on;
}

std::mutex &meshMutex() {
  static std::mutex m;
  return m;
}

void quietMessenger() {
  static std::once_flag once;
  std::call_once(once, [] {
    Handle(Message_Messenger) msgr = Message::DefaultMessenger();
    if (!msgr.IsNull()) {
      Message_SequenceOfPrinters &printers = msgr->ChangePrinters();
      printers.Clear();
    }
  });
}

TopoDS_Shape doBoolean(const TopoDS_Shape &a, const TopoDS_Shape &b, uint32_t op) {
  TopTools_ListOfShape args, tools;
  args.Append(a);
  tools.Append(b);
  std::unique_ptr<BRepAlgoAPI_BooleanOperation> alg;
  switch (op) {
  case 0: alg.reset(new BRepAlgoAPI_Fuse()); break;
  case 1: alg.reset(new BRepAlgoAPI_Cut()); break;
  case 2: alg.reset(new BRepAlgoAPI_Common()); break;
  default: fail("unknown boolean op");
  }
  alg->SetArguments(args);
  alg->SetTools(tools);
  alg->SetNonDestructive(Standard_True);
  alg->SetRunParallel(parallelBop());
  alg->SetUseOBB(parallelBop());
  alg->Build();
  if (!alg->IsDone() || alg->HasErrors()) {
    std::ostringstream ss;
    alg->DumpErrors(ss);
    fail("boolean operation failed: " + ss.str());
  }
  return alg->Shape();
}

TopoDS_Shape doUnify(const TopoDS_Shape &s) {
  if (s.IsNull()) fail("unify: null shape");
  ShapeUpgrade_UnifySameDomain u(s, Standard_True, Standard_True, Standard_False);
  u.Build();
  TopoDS_Shape r = u.Shape();
  if (r.IsNull()) fail("unify produced no shape");
  return r;
}

TopoDS_Shape transformed(const TopoDS_Shape &s, const gp_Trsf &t) {
  BRepBuilderAPI_Transform tr(s, t, Standard_True);
  if (!tr.IsDone()) fail("transform failed");
  return tr.Shape();
}

TopoDS_Edge edgeFromSpec(const EdgeSpec &e) {
  switch (e.kind) {
  case 0: {
    gp_Pnt a = P(e.p0), b = P(e.p1);
    if (a.Distance(b) < kTol) fail("degenerate line segment");
    BRepBuilderAPI_MakeEdge me(a, b);
    if (!me.IsDone()) fail("cannot build line edge");
    return me.Edge();
  }
  case 1: {
    GC_MakeArcOfCircle arc(P(e.p0), P(e.p1), P(e.p2));
    if (!arc.IsDone()) fail("cannot build arc (collinear points?)");
    BRepBuilderAPI_MakeEdge me(arc.Value());
    if (!me.IsDone()) fail("cannot build arc edge");
    return me.Edge();
  }
  case 2: {
    if (e.radius <= kTol) fail("circle radius must be positive");
    gp_Circ c(gp_Ax2(P(e.p0), D(e.p1, "circle normal")), e.radius);
    BRepBuilderAPI_MakeEdge me(c);
    if (!me.IsDone()) fail("cannot build circle edge");
    return me.Edge();
  }
  default: fail("unknown edge kind");
  }
}

TopoDS_Wire wireFrom(const std::vector<TopoDS_Edge> &edges) {
  BRepBuilderAPI_MakeWire mw;
  for (const auto &e : edges) {
    mw.Add(e);
    if (!mw.IsDone()) {
      // Fall back to a tolerant build with ShapeFix.
      break;
    }
  }
  TopoDS_Wire w;
  if (mw.IsDone()) {
    w = mw.Wire();
  } else {
    BRep_Builder bb;
    bb.MakeWire(w);
    for (const auto &e : edges) bb.Add(w, e);
    ShapeFix_Wire sfw;
    sfw.Load(w);
    sfw.SetPrecision(1e-4);
    sfw.FixReorder();
    sfw.FixConnected(1e-3);
    w = sfw.Wire();
  }
  if (!BRep_Tool::IsClosed(w)) fail("profile loop is not closed");
  return w;
}

} // namespace

// ---------------------------------------------------------------- basics

std::unique_ptr<Shape> shape_clone(const Shape &s) { return std::make_unique<Shape>(s.s); }
bool shape_is_null(const Shape &s) { return s.s.IsNull(); }
uint32_t shape_type(const Shape &s) { return s.s.IsNull() ? 8u : static_cast<uint32_t>(s.s.ShapeType()); }

bool shape_is_valid(const Shape &s) {
  if (s.s.IsNull()) return false;
  try {
    BRepCheck_Analyzer an(s.s);
    return an.IsValid();
  } catch (...) {
    return false;
  }
}

uint32_t count_sub(const Shape &s, uint32_t kind) {
  if (s.s.IsNull() || kind > 7) return 0;
  return static_cast<uint32_t>(mapOf(s.s, static_cast<TopAbs_ShapeEnum>(kind)).Extent());
}

std::unique_ptr<Shape> sub_shape(const Shape &s, uint32_t kind, uint32_t index) {
  requireShape(s, "sub_shape");
  if (kind > 7) fail("bad shape kind");
  auto m = mapOf(s.s, static_cast<TopAbs_ShapeEnum>(kind));
  if (index >= static_cast<uint32_t>(m.Extent())) fail("sub-shape index out of range");
  return wrap(m.FindKey(static_cast<int>(index) + 1));
}

std::unique_ptr<Shape> compound_new() {
  TopoDS_Compound c;
  BRep_Builder b;
  b.MakeCompound(c);
  return std::make_unique<Shape>(c);
}

void compound_add(Shape &c, const Shape &s) {
  if (c.s.IsNull() || c.s.ShapeType() != TopAbs_COMPOUND) fail("compound_add: target is not a compound");
  requireShape(s, "compound_add");
  BRep_Builder b;
  b.Add(c.s, s.s);
}

// ---------------------------------------------------------------- primitives

std::unique_ptr<Shape> make_box(V3 origin, double dx, double dy, double dz) {
  if (dx <= 0 || dy <= 0 || dz <= 0) fail("box dimensions must be positive");
  BRepPrimAPI_MakeBox mb(P(origin), dx, dy, dz);
  return wrap(mb.Solid());
}

std::unique_ptr<Shape> make_cylinder(V3 base, V3 dir, double r, double h) {
  if (r <= 0 || h <= 0) fail("cylinder radius/height must be positive");
  BRepPrimAPI_MakeCylinder mc(gp_Ax2(P(base), D(dir, "cylinder axis")), r, h);
  return wrap(mc.Solid());
}

std::unique_ptr<Shape> make_cone(V3 base, V3 dir, double r1, double r2, double h) {
  if (r1 < 0 || r2 < 0 || h <= 0 || (r1 == 0 && r2 == 0)) fail("bad cone dimensions");
  BRepPrimAPI_MakeCone mc(gp_Ax2(P(base), D(dir, "cone axis")), r1, r2, h);
  return wrap(mc.Solid());
}

// ---------------------------------------------------------------- profiles

std::unique_ptr<Shape> face_from_edges(const Frame &frame, rust::Slice<const EdgeSpec> edges) {
  if (edges.empty()) fail("profile has no edges");
  std::vector<std::vector<TopoDS_Edge>> loops;
  uint32_t current = UINT32_MAX;
  for (const EdgeSpec &e : edges) {
    if (e.loop_id != current) {
      loops.emplace_back();
      current = e.loop_id;
    }
    loops.back().push_back(edgeFromSpec(e));
  }
  gp_Pln pln(frameAx(frame));
  BRepBuilderAPI_MakeFace mf(pln, wireFrom(loops[0]), Standard_True);
  if (!mf.IsDone()) fail("cannot build face from outer loop");
  for (size_t i = 1; i < loops.size(); ++i) {
    TopoDS_Wire w = wireFrom(loops[i]);
    mf.Add(TopoDS::Wire(w.Reversed()));
  }
  TopoDS_Face f = mf.Face();
  ShapeFix_Face sff(f);
  sff.FixOrientationMode() = 1;
  sff.Perform();
  f = sff.Face();
  // Keep the face normal aligned with the frame normal.
  BRepAdaptor_Surface as(f);
  gp_Dir n = as.Plane().Axis().Direction();
  if (f.Orientation() == TopAbs_REVERSED) n.Reverse();
  if (n.Dot(D(frame.normal, "frame normal")) < 0) f.Reverse();
  BRepCheck_Analyzer an(f);
  if (!an.IsValid()) fail("profile face is invalid (self-intersecting or overlapping loops?)");
  return wrap(f);
}

std::unique_ptr<Shape> extrude(const Shape &s, V3 v) {
  requireShape(s, "extrude");
  gp_Vec vec = Vv(v);
  if (vec.Magnitude() < kTol) fail("extrude distance must be non-zero");
  BRepPrimAPI_MakePrism mp(s.s, vec, Standard_True);
  if (!mp.IsDone()) fail("extrude failed");
  return wrap(simplify(mp.Shape()));
}

std::unique_ptr<Shape> extrude_draft(const Shape &s, V3 v, double angle_rad, const Frame &neutral) {
  requireShape(s, "extrude_draft");
  gp_Vec vec = Vv(v);
  if (vec.Magnitude() < kTol) fail("extrude distance must be non-zero");
  BRepPrimAPI_MakePrism mp(s.s, vec, Standard_True);
  if (!mp.IsDone()) fail("extrude failed");
  TopoDS_Shape prism = mp.Shape();
  if (std::abs(angle_rad) < 1e-12) return wrap(simplify(prism));
  // Lateral faces = all faces not coming from the base/top caps.
  TopTools_IndexedMapOfShape caps;
  TopExp::MapShapes(mp.FirstShape(), TopAbs_FACE, caps);
  TopExp::MapShapes(mp.LastShape(), TopAbs_FACE, caps);
  BRepOffsetAPI_DraftAngle dr(prism);
  gp_Dir pull(vec);
  gp_Pln pln(frameAx(neutral));
  int added = 0;
  for (TopExp_Explorer ex(prism, TopAbs_FACE); ex.More(); ex.Next()) {
    if (caps.Contains(ex.Current())) continue;
    dr.Add(TopoDS::Face(ex.Current()), pull, angle_rad, pln);
    if (!dr.AddDone()) fail("draft could not be applied to an extrusion side face");
    ++added;
  }
  if (added == 0) return wrap(simplify(prism));
  dr.Build();
  if (!dr.IsDone()) fail("drafted extrusion failed");
  TopoDS_Shape r = healIfInvalid(dr.Shape(), "drafted extrusion");
  checkDraftCones(r, "drafted extrusion");
  return wrap(simplify(r));
}

std::unique_ptr<Shape> revolve(const Shape &s, V3 origin, V3 dir, double angle_rad) {
  requireShape(s, "revolve");
  if (std::abs(angle_rad) < 1e-9) fail("revolve angle must be non-zero");
  gp_Ax1 ax(P(origin), D(dir, "revolve axis"));
  BRepPrimAPI_MakeRevol mr(s.s, ax, angle_rad, Standard_True);
  if (!mr.IsDone()) fail("revolve failed");
  return wrap(simplify(mr.Shape()));
}

// ---------------------------------------------------------------- booleans

std::unique_ptr<Shape> boolean(const Shape &a, const Shape &b, uint32_t op) {
  requireShape(a, "boolean");
  requireShape(b, "boolean");
  return wrap(simplify(doBoolean(a.s, b.s, op)));
}

std::unique_ptr<Shape> fuse_all(const Shape &c) {
  requireShape(c, "fuse_all");
  std::vector<TopoDS_Shape> kids;
  for (TopoDS_Iterator it(c.s); it.More(); it.Next()) kids.push_back(it.Value());
  if (kids.empty()) fail("fuse_all: nothing to fuse");
  if (kids.size() == 1) return wrap(simplify(kids[0]));
  TopTools_ListOfShape args, tools;
  args.Append(kids[0]);
  for (size_t i = 1; i < kids.size(); ++i) tools.Append(kids[i]);
  BRepAlgoAPI_Fuse f;
  f.SetArguments(args);
  f.SetTools(tools);
  f.SetNonDestructive(Standard_True);
  f.SetRunParallel(parallelBop());
  f.SetUseOBB(parallelBop());
  f.Build();
  if (!f.IsDone() || f.HasErrors()) {
    std::ostringstream ss;
    f.DumpErrors(ss);
    fail("fuse failed: " + ss.str());
  }
  return wrap(simplify(f.Shape()));
}

std::unique_ptr<Shape> unify(const Shape &s) {
  requireShape(s, "unify");
  return wrap(simplify(doUnify(s.s)));
}

std::unique_ptr<Shape> heal(const Shape &s) {
  requireShape(s, "heal");
  Handle(ShapeFix_Shape) fix = new ShapeFix_Shape(s.s);
  fix->Perform();
  TopoDS_Shape r = fix->Shape();
  if (r.IsNull()) fail("heal produced no shape");
  r = doUnify(r);
  return wrap(simplify(r));
}

namespace {
bool tangentAtVertex(const TopoDS_Edge &e, const TopoDS_Vertex &v, gp_Vec &t) {
  if (BRep_Tool::Degenerated(e)) return false;
  BRepAdaptor_Curve c(e);
  double par = BRep_Tool::Parameter(v, e);
  gp_Pnt p;
  c.D1(par, p, t);
  return t.Magnitude() > 1e-12;
}

int findRoot(std::vector<int> &parent, int i) {
  while (parent[i] != i) {
    parent[i] = parent[parent[i]];
    i = parent[i];
  }
  return i;
}

bool normalOnFace(const TopoDS_Face &f, const TopoDS_Edge &e, double t, gp_Vec &n) {
  BRepAdaptor_Curve2d c2(e, f);
  gp_Pnt2d uv = c2.Value(t);
  BRepAdaptor_Surface as(f, Standard_False);
  gp_Pnt p;
  gp_Vec du, dv;
  as.D1(uv.X(), uv.Y(), p, du, dv);
  n = du.Crossed(dv);
  if (n.Magnitude() < 1e-12) return false;
  n.Normalize();
  if (f.Orientation() == TopAbs_REVERSED) n.Reverse();
  return true;
}

// Orientation of edge `e` within face `f` (FORWARD/REVERSED), or EXTERNAL when absent/seam.
TopAbs_Orientation edgeOrientationIn(const TopoDS_Face &f, const TopoDS_Edge &e) {
  TopAbs_Orientation o = TopAbs_EXTERNAL;
  int hits = 0;
  for (TopExp_Explorer ex(f, TopAbs_EDGE); ex.More(); ex.Next()) {
    if (ex.Current().IsSame(e)) {
      o = ex.Current().Orientation();
      ++hits;
    }
  }
  return hits == 1 ? o : TopAbs_EXTERNAL;
}
} // namespace

rust::Vec<uint32_t> tangent_chains(const Shape &s) {
  requireShape(s, "tangent_chains");
  auto em = mapOf(s.s, TopAbs_EDGE);
  int n = em.Extent();
  std::vector<int> parent(static_cast<size_t>(n));
  for (int i = 0; i < n; ++i) parent[static_cast<size_t>(i)] = i;
  TopTools_IndexedDataMapOfShapeListOfShape v2e;
  TopExp::MapShapesAndAncestors(s.s, TopAbs_VERTEX, TopAbs_EDGE, v2e);
  for (int vi = 1; vi <= v2e.Extent(); ++vi) {
    TopoDS_Vertex v = TopoDS::Vertex(v2e.FindKey(vi));
    std::vector<int> ids;
    for (auto it = v2e.FindFromIndex(vi).cbegin(); it != v2e.FindFromIndex(vi).cend(); ++it) {
      int id = em.FindIndex(*it) - 1;
      if (id >= 0 && std::find(ids.begin(), ids.end(), id) == ids.end()) ids.push_back(id);
    }
    for (size_t a = 0; a < ids.size(); ++a) {
      for (size_t b = a + 1; b < ids.size(); ++b) {
        gp_Vec ta, tb;
        if (!tangentAtVertex(TopoDS::Edge(em.FindKey(ids[a] + 1)), v, ta)) continue;
        if (!tangentAtVertex(TopoDS::Edge(em.FindKey(ids[b] + 1)), v, tb)) continue;
        if (std::abs(ta.Normalized().Dot(tb.Normalized())) > std::cos(1.0 * M_PI / 180.0)) {
          int ra = findRoot(parent, ids[a]), rb = findRoot(parent, ids[b]);
          if (ra != rb) parent[static_cast<size_t>(ra)] = rb;
        }
      }
    }
  }
  rust::Vec<uint32_t> out;
  for (int i = 0; i < n; ++i) out.push_back(static_cast<uint32_t>(findRoot(parent, i)));
  return out;
}

int32_t edge_convexity(const Shape &s, uint32_t edge) {
  requireShape(s, "edge_convexity");
  auto em = mapOf(s.s, TopAbs_EDGE);
  if (edge >= static_cast<uint32_t>(em.Extent())) fail("edge index out of range");
  TopoDS_Edge e = TopoDS::Edge(em.FindKey(static_cast<int>(edge) + 1));
  if (BRep_Tool::Degenerated(e)) return 0;
  TopTools_IndexedDataMapOfShapeListOfShape anc;
  TopExp::MapShapesAndAncestors(s.s, TopAbs_EDGE, TopAbs_FACE, anc);
  if (!anc.Contains(e)) return 0;
  std::vector<TopoDS_Face> fs;
  for (auto it = anc.FindFromKey(e).cbegin(); it != anc.FindFromKey(e).cend(); ++it) {
    bool dup = false;
    for (auto &f : fs) dup = dup || f.IsSame(*it);
    if (!dup) fs.push_back(TopoDS::Face(*it));
  }
  if (fs.size() != 2) return 0;
  BRepAdaptor_Curve c(e);
  double t = (c.FirstParameter() + c.LastParameter()) / 2;
  gp_Pnt p;
  gp_Vec tan;
  c.D1(t, p, tan);
  if (tan.Magnitude() < 1e-12) return 0;
  TopAbs_Orientation o1 = edgeOrientationIn(fs[0], e);
  if (o1 == TopAbs_EXTERNAL) return 0;
  if (o1 == TopAbs_REVERSED) tan.Reverse();
  gp_Vec n1, n2;
  if (!normalOnFace(fs[0], e, t, n1) || !normalOnFace(fs[1], e, t, n2)) return 0;
  gp_Vec cr = n1.Crossed(n2);
  if (cr.Magnitude() < 1e-6) return 0;
  return cr.Dot(tan) > 0 ? 1 : -1;
}

int32_t match_edge(const Shape &s, V3 p, V3 tangent, double tol) {
  requireShape(s, "match_edge");
  auto em = mapOf(s.s, TopAbs_EDGE);
  gp_Pnt pt = P(p);
  gp_Vec want = Vv(tangent);
  bool useTan = want.Magnitude() > 1e-12;
  if (useTan) want.Normalize();
  int best = -1;
  double bestScore = 1e300;
  ShapeAnalysis_Curve sac;
  for (int i = 1; i <= em.Extent(); ++i) {
    TopoDS_Edge e = TopoDS::Edge(em.FindKey(i));
    if (BRep_Tool::Degenerated(e)) continue;
    BRepAdaptor_Curve c(e);
    gp_Pnt proj;
    double par = 0;
    double d = sac.Project(c, pt, 1e-7, proj, par, Standard_True);
    if (d > tol) continue;
    double score = d;
    if (useTan) {
      gp_Pnt q;
      gp_Vec t;
      c.D1(par, q, t);
      if (t.Magnitude() < 1e-12) continue;
      double dot = std::abs(t.Normalized().Dot(want));
      if (dot < 0.98) continue;
      score += (1.0 - dot) * tol;
    }
    if (score < bestScore) {
      bestScore = score;
      best = i - 1;
    }
  }
  return best;
}

// ---------------------------------------------------------------- dress-up

std::unique_ptr<Shape> fillet(const Shape &s, rust::Slice<const uint32_t> edges, rust::Slice<const double> radii) {
  requireShape(s, "fillet");
  if (edges.empty()) fail("fillet: no edges selected");
  if (radii.size() != edges.size()) fail("fillet: one radius per edge required");
  auto m = mapOf(s.s, TopAbs_EDGE);
  BRepFilletAPI_MakeFillet mf(s.s);
  for (size_t k = 0; k < edges.size(); ++k) {
    uint32_t i = edges[k];
    if (i >= static_cast<uint32_t>(m.Extent())) fail("fillet: edge index out of range");
    if (radii[k] <= 0) fail("fillet radius must be positive");
    mf.Add(radii[k], TopoDS::Edge(m.FindKey(static_cast<int>(i) + 1)));
  }
  mf.Build();
  if (!mf.IsDone()) fail("fillet failed (radius too large for the adjacent faces?)");
  TopoDS_Shape r = healIfInvalid(mf.Shape(), "fillet");
  return wrap(simplify(r));
}

std::unique_ptr<Shape> chamfer(const Shape &s, rust::Slice<const uint32_t> edges, rust::Slice<const double> dists) {
  requireShape(s, "chamfer");
  if (edges.empty()) fail("chamfer: no edges selected");
  if (dists.size() != edges.size()) fail("chamfer: one distance per edge required");
  auto m = mapOf(s.s, TopAbs_EDGE);
  BRepFilletAPI_MakeChamfer mc(s.s);
  for (size_t k = 0; k < edges.size(); ++k) {
    uint32_t i = edges[k];
    if (i >= static_cast<uint32_t>(m.Extent())) fail("chamfer: edge index out of range");
    if (dists[k] <= 0) fail("chamfer distance must be positive");
    mc.Add(dists[k], TopoDS::Edge(m.FindKey(static_cast<int>(i) + 1)));
  }
  mc.Build();
  if (!mc.IsDone()) fail("chamfer failed (distance too large?)");
  TopoDS_Shape r = healIfInvalid(mc.Shape(), "chamfer");
  return wrap(simplify(r));
}

std::unique_ptr<Shape> shell(const Shape &s, rust::Slice<const uint32_t> faces, double offset) {
  requireShape(s, "shell");
  if (std::abs(offset) < kTol) fail("shell thickness must be non-zero");
  auto m = mapOf(s.s, TopAbs_FACE);
  TopTools_ListOfShape closing;
  for (uint32_t i : faces) {
    if (i >= static_cast<uint32_t>(m.Extent())) fail("shell: face index out of range");
    closing.Append(m.FindKey(static_cast<int>(i) + 1));
  }
  // The thick-solid algorithm wants a single solid.
  TopoDS_Shape input = simplify(s.s);
  BRepOffsetAPI_MakeThickSolid mk;
  mk.MakeThickSolidByJoin(input, closing, offset, 1e-3, BRepOffset_Skin, Standard_False,
                          Standard_False, GeomAbs_Arc);
  mk.Build();
  if (!mk.IsDone()) fail("shell failed (thickness larger than a fillet radius or feature?)");
  TopoDS_Shape r = healIfInvalid(mk.Shape(), "shell");
  return wrap(simplify(r));
}

std::unique_ptr<Shape> draft(const Shape &s, rust::Slice<const uint32_t> faces, V3 pull,
                             double angle_rad, const Frame &neutral) {
  requireShape(s, "draft");
  if (faces.empty()) fail("draft: no faces selected");
  auto m = mapOf(s.s, TopAbs_FACE);
  BRepOffsetAPI_DraftAngle dr(s.s);
  gp_Dir dir = D(pull, "pull direction");
  gp_Pln pln(frameAx(neutral));
  for (uint32_t i : faces) {
    if (i >= static_cast<uint32_t>(m.Extent())) fail("draft: face index out of range");
    dr.Add(TopoDS::Face(m.FindKey(static_cast<int>(i) + 1)), dir, angle_rad, pln);
    if (!dr.AddDone()) fail("draft: face " + std::to_string(i) + " cannot be drafted");
  }
  dr.Build();
  if (!dr.IsDone()) fail("draft failed");
  TopoDS_Shape r = healIfInvalid(dr.Shape(), "draft");
  checkDraftCones(r, "draft");
  return wrap(simplify(r));
}

// ---------------------------------------------------------------- transforms

std::unique_ptr<Shape> translate(const Shape &s, V3 v) {
  requireShape(s, "translate");
  gp_Trsf t;
  t.SetTranslation(Vv(v));
  return wrap(transformed(s.s, t));
}

std::unique_ptr<Shape> rotate(const Shape &s, V3 origin, V3 dir, double angle_rad) {
  requireShape(s, "rotate");
  gp_Trsf t;
  t.SetRotation(gp_Ax1(P(origin), D(dir, "rotation axis")), angle_rad);
  return wrap(transformed(s.s, t));
}

std::unique_ptr<Shape> mirror(const Shape &s, V3 origin, V3 normal) {
  requireShape(s, "mirror");
  gp_Trsf t;
  t.SetMirror(gp_Ax2(P(origin), D(normal, "mirror normal")));
  return wrap(transformed(s.s, t));
}

std::unique_ptr<Shape> half_space_side(const Shape &s, V3 origin, V3 normal, bool above) {
  requireShape(s, "split");
  Bnd_Box bb;
  BRepBndLib::Add(s.s, bb);
  if (bb.IsVoid()) fail("split: empty shape");
  double L = 2.0 * std::sqrt(bb.SquareExtent()) + 10.0;
  // Translate the plane origin to the projection of the bbox centre so the cutting
  // box always covers the shape.
  double xmin, ymin, zmin, xmax, ymax, zmax;
  bb.Get(xmin, ymin, zmin, xmax, ymax, zmax);
  gp_Pnt c((xmin + xmax) / 2, (ymin + ymax) / 2, (zmin + zmax) / 2);
  gp_Dir n = D(normal, "split normal");
  gp_Pnt o = P(origin);
  double dist = gp_Vec(o, c).Dot(gp_Vec(n));
  gp_Pnt oc = c.Translated(gp_Vec(n) * -dist); // centre projected onto plane
  gp_Ax2 ax(oc, n);
  gp_Vec x(ax.XDirection()), y(ax.YDirection());
  gp_Pnt corner = oc.Translated(x * -L).Translated(y * -L);
  BRepPrimAPI_MakeBox mb(gp_Ax2(corner, n, ax.XDirection()), 2 * L, 2 * L, L);
  TopoDS_Shape halfBox = mb.Solid();
  TopoDS_Shape r = doBoolean(s.s, halfBox, above ? 2u : 1u);
  r = simplify(r);
  if (count_sub(Shape(r), TopAbs_SOLID) == 0) fail("split: nothing on that side of the plane");
  return wrap(r);
}

// ---------------------------------------------------------------- planar regions

namespace {
double faceArea(const TopoDS_Face &f) {
  GProp_GProps g;
  BRepGProp::SurfaceProperties(f, g);
  return std::abs(g.Mass());
}

std::vector<TopoDS_Face> facesOf(const TopoDS_Shape &s) {
  std::vector<TopoDS_Face> v;
  for (TopExp_Explorer ex(s, TopAbs_FACE); ex.More(); ex.Next()) v.push_back(TopoDS::Face(ex.Current()));
  return v;
}

TopoDS_Face largestFace(const TopoDS_Shape &s) {
  TopoDS_Face best;
  double ba = -1;
  for (auto &f : facesOf(s)) {
    double a = faceArea(f);
    if (a > ba) {
      ba = a;
      best = f;
    }
  }
  if (best.IsNull()) fail("no face");
  return best;
}

gp_Pln planeOf(const TopoDS_Face &f) {
  BRepAdaptor_Surface as(f);
  if (as.GetType() != GeomAbs_Plane) fail("face is not planar");
  return as.Plane();
}

bool inFace(const TopoDS_Face &f, const gp_Pnt &p) {
  BRepClass_FaceClassifier fc(f, p, 1e-6);
  return fc.State() == TopAbs_IN || fc.State() == TopAbs_ON;
}

// Build faces on `pln` from closed wires, nesting holes by containment.
TopoDS_Shape facesFromWires(const gp_Pln &pln, const std::vector<TopoDS_Wire> &wires, const gp_Dir &normal) {
  struct W {
    TopoDS_Wire w;
    TopoDS_Face f;
    double area;
    int depth = 0;
    int parent = -1;
  };
  std::vector<W> ws;
  for (const auto &w : wires) {
    BRepBuilderAPI_MakeFace mf(pln, w, Standard_True);
    if (!mf.IsDone()) continue;
    TopoDS_Face f = mf.Face();
    ShapeFix_Face sff(f);
    sff.Perform();
    f = sff.Face();
    double a = faceArea(f);
    if (a < 1e-9) continue;
    ws.push_back({w, f, a});
  }
  std::sort(ws.begin(), ws.end(), [](const W &a, const W &b) { return a.area > b.area; });
  for (size_t i = 0; i < ws.size(); ++i) {
    TopExp_Explorer vx(ws[i].w, TopAbs_VERTEX);
    if (!vx.More()) continue;
    gp_Pnt p = BRep_Tool::Pnt(TopoDS::Vertex(vx.Current()));
    for (size_t j = 0; j < i; ++j) {
      if (inFace(ws[j].f, p)) {
        // smallest containing (sorted by area desc → last match is smallest)
        ws[i].parent = static_cast<int>(j);
      }
    }
  }
  for (size_t i = 0; i < ws.size(); ++i) {
    int d = 0;
    for (int p = ws[i].parent; p >= 0; p = ws[static_cast<size_t>(p)].parent) ++d;
    ws[i].depth = d;
  }
  TopoDS_Compound c;
  BRep_Builder bb;
  bb.MakeCompound(c);
  int n = 0;
  for (size_t i = 0; i < ws.size(); ++i) {
    if (ws[i].depth % 2) continue;
    TopoDS_Face f = ws[i].f;
    gp_Dir fn = BRepAdaptor_Surface(f).Plane().Axis().Direction();
    if (f.Orientation() == TopAbs_REVERSED) fn.Reverse();
    if (fn.Dot(normal) < 0) f.Reverse();
    for (size_t j = 0; j < ws.size(); ++j) {
      if (ws[j].parent == static_cast<int>(i) && ws[j].depth % 2 == 1) {
        TopoDS_Shape r = doBoolean(f, ws[j].f, 1u);
        TopExp_Explorer fx(r, TopAbs_FACE);
        if (!fx.More()) fail("hole removal failed");
        f = TopoDS::Face(fx.Current());
      }
    }
    bb.Add(c, f);
    ++n;
  }
  if (n == 0) fail("offset region vanished");
  return c;
}
} // namespace

std::unique_ptr<Shape> section_face(const Shape &s, V3 origin, V3 normal) {
  requireShape(s, "section_face");
  Bnd_Box bb;
  BRepBndLib::Add(s.s, bb);
  if (bb.IsVoid()) fail("section: empty shape");
  double L = 2.0 * std::sqrt(bb.SquareExtent()) + 10.0;
  gp_Pln pln(P(origin), D(normal, "section normal"));
  TopoDS_Face big = BRepBuilderAPI_MakeFace(pln, -L, L, -L, L).Face();
  TopoDS_Shape r = doBoolean(s.s, big, 2u);
  r = doUnify(r);
  if (facesOf(r).empty()) fail("the plane does not cut the body");
  return wrap(r);
}

std::unique_ptr<Shape> offset_face(const Shape &s, double d) {
  requireShape(s, "offset_face");
  std::vector<TopoDS_Wire> wires;
  gp_Pln pln;
  gp_Dir normal(0, 0, 1);
  bool first = true;
  for (auto &f : facesOf(s.s)) {
    gp_Pln p = planeOf(f);
    gp_Dir n = p.Axis().Direction();
    if (f.Orientation() == TopAbs_REVERSED) n.Reverse();
    if (first) {
      pln = gp_Pln(p.Location(), n);
      normal = n;
      first = false;
    }
    if (std::abs(d) < 1e-12) {
      for (TopExp_Explorer ex(f, TopAbs_WIRE); ex.More(); ex.Next()) wires.push_back(TopoDS::Wire(ex.Current()));
      continue;
    }
    BRepOffsetAPI_MakeOffset mo(f, GeomAbs_Arc, Standard_False);
    mo.Perform(d);
    if (!mo.IsDone()) fail("2D offset failed");
    for (TopExp_Explorer ex(mo.Shape(), TopAbs_WIRE); ex.More(); ex.Next()) {
      TopoDS_Wire w = TopoDS::Wire(ex.Current());
      if (BRep_Tool::IsClosed(w)) wires.push_back(w);
    }
  }
  if (first) fail("offset_face: no planar face");
  if (wires.empty()) fail("offset region vanished");
  return wrap(facesFromWires(pln, wires, normal));
}

std::unique_ptr<Shape> fill_holes(const Shape &s) {
  requireShape(s, "fill_holes");
  TopoDS_Compound c;
  BRep_Builder bb;
  bb.MakeCompound(c);
  for (auto &f : facesOf(s.s)) {
    TopoDS_Wire ow = BRepTools::OuterWire(f);
    BRepBuilderAPI_MakeFace mf(planeOf(f), ow, Standard_True);
    if (!mf.IsDone()) fail("fill_holes failed");
    TopoDS_Face nf = mf.Face();
    ShapeFix_Face sff(nf);
    sff.Perform();
    nf = sff.Face();
    if (nf.Orientation() != f.Orientation()) nf.Reverse();
    bb.Add(c, nf);
  }
  return wrap(c);
}

bool face_contains(const Shape &s, V3 p) {
  requireShape(s, "face_contains");
  for (auto &f : facesOf(s.s))
    if (inFace(f, P(p))) return true;
  return false;
}

double outer_wire_length(const Shape &s) {
  requireShape(s, "outer_wire_length");
  BRepAdaptor_CompCurve cc(BRepTools::OuterWire(largestFace(s.s)), Standard_True);
  return GCPnts_AbscissaPoint::Length(cc);
}

rust::Vec<V3> outer_wire_point(const Shape &s, double t) {
  requireShape(s, "outer_wire_point");
  BRepAdaptor_CompCurve cc(BRepTools::OuterWire(largestFace(s.s)), Standard_True);
  double L = GCPnts_AbscissaPoint::Length(cc);
  double tt = t - std::floor(t);
  double u0 = cc.FirstParameter();
  double u = u0;
  if (tt > 1e-12) {
    GCPnts_AbscissaPoint ap(cc, tt * L, u0);
    if (!ap.IsDone()) fail("wire parameterisation failed");
    u = ap.Parameter();
  }
  gp_Pnt p;
  gp_Vec v;
  cc.D1(u, p, v);
  if (v.Magnitude() < 1e-12) fail("degenerate tangent");
  rust::Vec<V3> out;
  out.push_back(toV(p));
  out.push_back(toV(gp_Dir(v)));
  return out;
}

std::unique_ptr<Shape> offset_shape(const Shape &s, double d) {
  requireShape(s, "offset_shape");
  BRepOffsetAPI_MakeOffsetShape mo;
  mo.PerformByJoin(s.s, d, 1e-4, BRepOffset_Skin, Standard_False, Standard_False, GeomAbs_Arc);
  if (!mo.IsDone()) fail("3D offset failed");
  TopoDS_Shape r = mo.Shape();
  if (r.IsNull()) fail("3D offset produced no shape (offset larger than a convex radius?)");
  // an offset of a solid's skin yields a shell or solid; make it a solid
  if (r.ShapeType() == TopAbs_SHELL) {
    BRep_Builder bb;
    TopoDS_Solid so;
    bb.MakeSolid(so);
    bb.Add(so, r);
    r = so;
  }
  return wrap(simplify(healIfInvalid(r, "offset")));
}

// ---------------------------------------------------------------- queries

rust::Vec<FaceInfo> faces(const Shape &s) {
  requireShape(s, "faces");
  rust::Vec<FaceInfo> out;
  auto m = mapOf(s.s, TopAbs_FACE);
  for (int i = 1; i <= m.Extent(); ++i) {
    TopoDS_Face f = TopoDS::Face(m.FindKey(i));
    FaceInfo fi{};
    fi.index = static_cast<uint32_t>(i - 1);
    GProp_GProps props;
    BRepGProp::SurfaceProperties(f, props);
    fi.area = props.Mass();
    gp_Pnt cen = props.CentreOfMass();
    fi.centroid = toV(cen);
    BRepAdaptor_Surface as(f, Standard_True);
    switch (as.GetType()) {
    case GeomAbs_Plane: fi.kind = 0; break;
    case GeomAbs_Cylinder: {
      fi.kind = 1;
      gp_Cylinder cy = as.Cylinder();
      fi.axis_origin = toV(cy.Axis().Location());
      fi.axis_dir = toV(cy.Axis().Direction());
      fi.radius = cy.Radius();
      break;
    }
    case GeomAbs_Cone: {
      fi.kind = 2;
      gp_Cone co = as.Cone();
      fi.axis_origin = toV(co.Axis().Location());
      fi.axis_dir = toV(co.Axis().Direction());
      fi.radius = co.RefRadius();
      break;
    }
    case GeomAbs_Sphere: fi.kind = 3; fi.radius = as.Sphere().Radius(); break;
    case GeomAbs_Torus: fi.kind = 4; break;
    default: fi.kind = 5; break;
    }
    // Normal at the point of the surface closest to the centroid.
    double u0, u1, v0, v1;
    BRepTools::UVBounds(f, u0, u1, v0, v1);
    double u = (u0 + u1) / 2, v = (v0 + v1) / 2;
    Handle(Geom_Surface) surf = BRep_Tool::Surface(f);
    if (fi.kind != 0) {
      try {
        GeomAPI_ProjectPointOnSurf proj(cen, surf, u0, u1, v0, v1);
        if (proj.NbPoints() > 0) proj.LowerDistanceParameters(u, v);
      } catch (...) {
      }
    }
    GeomLProp_SLProps sl(surf, u, v, 1, 1e-9);
    if (sl.IsNormalDefined()) {
      gp_Dir n = sl.Normal();
      if (f.Orientation() == TopAbs_REVERSED) n.Reverse();
      fi.normal = toV(n);
    }
    fi.point = toV(surf->Value(u, v));
    out.push_back(fi);
  }
  return out;
}

rust::Vec<EdgeInfo> edges(const Shape &s) {
  requireShape(s, "edges");
  rust::Vec<EdgeInfo> out;
  auto fm = mapOf(s.s, TopAbs_FACE);
  TopTools_IndexedDataMapOfShapeListOfShape anc;
  TopExp::MapShapesAndAncestors(s.s, TopAbs_EDGE, TopAbs_FACE, anc);
  auto m = mapOf(s.s, TopAbs_EDGE);
  for (int i = 1; i <= m.Extent(); ++i) {
    TopoDS_Edge e = TopoDS::Edge(m.FindKey(i));
    EdgeInfo ei{};
    ei.index = static_cast<uint32_t>(i - 1);
    ei.face_a = -1;
    ei.face_b = -1;
    if (anc.Contains(e)) {
      const TopTools_ListOfShape &fl = anc.FindFromKey(e);
      for (auto it = fl.cbegin(); it != fl.cend(); ++it) {
        int fi = fm.FindIndex(*it) - 1;
        if (fi < 0) continue;
        if (ei.face_a < 0) ei.face_a = fi;
        else if (ei.face_b < 0 && fi != ei.face_a) ei.face_b = fi;
      }
    }
    if (BRep_Tool::Degenerated(e)) {
      ei.kind = 4;
      out.push_back(ei);
      continue;
    }
    BRepAdaptor_Curve c(e);
    double t0 = c.FirstParameter(), t1 = c.LastParameter();
    gp_Pnt pm;
    gp_Vec d1;
    c.D1((t0 + t1) / 2, pm, d1);
    ei.midpoint = toV(pm);
    if (d1.Magnitude() > 1e-12) ei.tangent = toV(gp_Dir(d1));
    ei.start = toV(c.Value(t0));
    ei.end = toV(c.Value(t1));
    ei.closed = c.Value(t0).Distance(c.Value(t1)) < 1e-6;
    ei.length = GCPnts_AbscissaPoint::Length(c);
    switch (c.GetType()) {
    case GeomAbs_Line: ei.kind = 0; break;
    case GeomAbs_Circle: {
      ei.kind = 1;
      gp_Circ ci = c.Circle();
      ei.center = toV(ci.Location());
      ei.axis = toV(ci.Axis().Direction());
      ei.radius = ci.Radius();
      break;
    }
    case GeomAbs_Ellipse: ei.kind = 2; break;
    case GeomAbs_BSplineCurve:
    case GeomAbs_BezierCurve: ei.kind = 3; break;
    default: ei.kind = 4; break;
    }
    out.push_back(ei);
  }
  return out;
}

rust::Vec<uint32_t> face_edges(const Shape &s, uint32_t face) {
  requireShape(s, "face_edges");
  auto fm = mapOf(s.s, TopAbs_FACE);
  if (face >= static_cast<uint32_t>(fm.Extent())) fail("face index out of range");
  auto em = mapOf(s.s, TopAbs_EDGE);
  rust::Vec<uint32_t> out;
  TopTools_IndexedMapOfShape seen;
  for (TopExp_Explorer ex(fm.FindKey(static_cast<int>(face) + 1), TopAbs_EDGE); ex.More(); ex.Next()) {
    if (seen.Contains(ex.Current())) continue;
    seen.Add(ex.Current());
    int idx = em.FindIndex(ex.Current());
    if (idx > 0) out.push_back(static_cast<uint32_t>(idx - 1));
  }
  return out;
}

MeshData tessellate(const Shape &s, double lin_defl, double ang_defl_rad) {
  requireShape(s, "tessellate");
  if (lin_defl <= 0 || ang_defl_rad <= 0) fail("deflection must be positive");
  std::lock_guard<std::mutex> lock(meshMutex());
  // BRepMesh never coarsens an existing finer triangulation: start clean so each
  // quality level gets its own density (callers cache meshes per level).
  BRepTools::Clean(s.s);
  BRepMesh_IncrementalMesh mesher(s.s, lin_defl, Standard_False, ang_defl_rad, parallelBop());
  if (!mesher.IsDone()) fail("meshing failed");
  MeshData md;
  auto fm = mapOf(s.s, TopAbs_FACE);
  for (int fi = 1; fi <= fm.Extent(); ++fi) {
    TopoDS_Face f = TopoDS::Face(fm.FindKey(fi));
    TopLoc_Location loc;
    Handle(Poly_Triangulation) tri = BRep_Tool::Triangulation(f, loc);
    if (tri.IsNull()) continue;
    if (!tri->HasNormals()) BRepLib_ToolTriangulatedShape::ComputeNormals(f, tri);
    const gp_Trsf &tr = loc.Transformation();
    bool rev = f.Orientation() == TopAbs_REVERSED;
    uint32_t base = static_cast<uint32_t>(md.positions.size() / 3);
    for (int i = 1; i <= tri->NbNodes(); ++i) {
      gp_Pnt p = tri->Node(i).Transformed(tr);
      md.positions.push_back(static_cast<float>(p.X()));
      md.positions.push_back(static_cast<float>(p.Y()));
      md.positions.push_back(static_cast<float>(p.Z()));
      gp_Dir n = tri->HasNormals() ? tri->Normal(i) : gp_Dir(0, 0, 1);
      n.Transform(tr);
      if (rev) n.Reverse();
      md.normals.push_back(static_cast<float>(n.X()));
      md.normals.push_back(static_cast<float>(n.Y()));
      md.normals.push_back(static_cast<float>(n.Z()));
    }
    for (int i = 1; i <= tri->NbTriangles(); ++i) {
      int a, b, c;
      tri->Triangle(i).Get(a, b, c);
      if (rev) std::swap(b, c);
      md.indices.push_back(base + static_cast<uint32_t>(a - 1));
      md.indices.push_back(base + static_cast<uint32_t>(b - 1));
      md.indices.push_back(base + static_cast<uint32_t>(c - 1));
      md.tri_face.push_back(static_cast<uint32_t>(fi - 1));
    }
  }
  auto em = mapOf(s.s, TopAbs_EDGE);
  for (int ei = 1; ei <= em.Extent(); ++ei) {
    TopoDS_Edge e = TopoDS::Edge(em.FindKey(ei));
    if (BRep_Tool::Degenerated(e)) continue;
    try {
      BRepAdaptor_Curve c(e);
      GCPnts_TangentialDeflection td(c, ang_defl_rad, lin_defl);
      uint32_t start = static_cast<uint32_t>(md.edge_points.size() / 3);
      for (int i = 1; i <= td.NbPoints(); ++i) {
        gp_Pnt p = td.Value(i);
        md.edge_points.push_back(static_cast<float>(p.X()));
        md.edge_points.push_back(static_cast<float>(p.Y()));
        md.edge_points.push_back(static_cast<float>(p.Z()));
      }
      md.edge_ranges.push_back(start);
      md.edge_ranges.push_back(static_cast<uint32_t>(td.NbPoints()));
      md.edge_ids.push_back(static_cast<uint32_t>(ei - 1));
    } catch (...) {
      // Skip edges that cannot be discretised; faces are what matter for display.
    }
  }
  return md;
}

namespace {
void hlrCollect(const TopoDS_Shape &c, bool visible, uint8_t kind, double defl, HlrData &out) {
  if (c.IsNull()) return;
  for (TopExp_Explorer ex(c, TopAbs_EDGE); ex.More(); ex.Next()) {
    TopoDS_Edge e = TopoDS::Edge(ex.Current());
    if (BRep_Tool::Degenerated(e)) continue;
    try {
      BRepAdaptor_Curve cv(e);
      GCPnts_TangentialDeflection td(cv, 0.1, defl);
      if (td.NbPoints() < 2) continue;
      uint32_t start = static_cast<uint32_t>(out.pts.size() / 2);
      for (int i = 1; i <= td.NbPoints(); ++i) {
        gp_Pnt p = td.Value(i);
        out.pts.push_back(p.X());
        out.pts.push_back(p.Y());
      }
      out.ranges.push_back(start);
      out.ranges.push_back(static_cast<uint32_t>(td.NbPoints()));
      out.flags.push_back(static_cast<uint8_t>((visible ? 1 : 0) | (kind << 1)));
    } catch (...) {
    }
  }
}
} // namespace

HlrData hlr(const Shape &s, V3 dir, V3 x_dir, bool exact, double defl) {
  requireShape(s, "hlr");
  if (defl <= 0) fail("hlr: deflection must be positive");
  gp_Ax2 ax(gp_Pnt(0, 0, 0), D(dir, "view direction"), D(x_dir, "view x direction"));
  HLRAlgo_Projector proj(ax);
  HlrData out;
  bool done = false;
  if (exact) {
    try {
      Handle(HLRBRep_Algo) algo = new HLRBRep_Algo();
      algo->Add(s.s);
      algo->Projector(proj);
      algo->Update();
      algo->Hide();
      HLRBRep_HLRToShape h(algo);
      hlrCollect(h.VCompound(), true, 0, defl, out);
      hlrCollect(h.Rg1LineVCompound(), true, 1, defl, out);
      hlrCollect(h.OutLineVCompound(), true, 2, defl, out);
      hlrCollect(h.HCompound(), false, 0, defl, out);
      hlrCollect(h.Rg1LineHCompound(), false, 1, defl, out);
      hlrCollect(h.OutLineHCompound(), false, 2, defl, out);
      done = !out.ranges.empty();
    } catch (...) {
      done = false;
    }
  }
  if (!done) {
    out = HlrData();
    out.poly_fallback = exact;
    {
      std::lock_guard<std::mutex> lock(meshMutex());
      BRepMesh_IncrementalMesh mesher(s.s, defl, Standard_False, 0.2, Standard_False);
    }
    Handle(HLRBRep_PolyAlgo) algo = new HLRBRep_PolyAlgo(s.s);
    algo->Projector(proj);
    algo->Update();
    HLRBRep_PolyHLRToShape h;
    h.Update(algo);
    hlrCollect(h.VCompound(), true, 0, defl, out);
    hlrCollect(h.Rg1LineVCompound(), true, 1, defl, out);
    hlrCollect(h.OutLineVCompound(), true, 2, defl, out);
    hlrCollect(h.HCompound(), false, 0, defl, out);
    hlrCollect(h.Rg1LineHCompound(), false, 1, defl, out);
    hlrCollect(h.OutLineHCompound(), false, 2, defl, out);
  }
  return out;
}

MassProps mass_props(const Shape &s) {
  requireShape(s, "mass_props");
  MassProps mp{};
  GProp_GProps vp;
  BRepGProp::VolumeProperties(s.s, vp);
  mp.volume = vp.Mass();
  mp.center = toV(vp.CentreOfMass());
  GProp_GProps sp;
  BRepGProp::SurfaceProperties(s.s, sp);
  mp.area = sp.Mass();
  return mp;
}

BBox bounding_box(const Shape &s) {
  requireShape(s, "bounding_box");
  Bnd_Box bb;
  BRepBndLib::AddOptimal(s.s, bb, Standard_False, Standard_False);
  if (bb.IsVoid()) fail("bounding box of empty shape");
  double x0, y0, z0, x1, y1, z1;
  bb.Get(x0, y0, z0, x1, y1, z1);
  BBox r{};
  r.min = mk(x0, y0, z0);
  r.max = mk(x1, y1, z1);
  return r;
}

// ---------------------------------------------------------------- I/O

namespace {
std::unique_ptr<Shape> finishStepRead(STEPControl_Reader &rd, IFSelect_ReturnStatus st) {
  if (st != IFSelect_RetDone) fail("STEP read failed (not a valid STEP file?)");
  rd.TransferRoots();
  if (rd.NbShapes() == 0) fail("STEP file contains no shapes");
  TopoDS_Shape sh = rd.OneShape();
  if (sh.IsNull()) fail("STEP file produced an empty shape");
  return wrap(sh);
}
} // namespace

std::unique_ptr<Shape> read_step_file(rust::Str path) {
  quietMessenger();
  STEPControl_Reader rd;
  std::string p(path);
  return finishStepRead(rd, rd.ReadFile(p.c_str()));
}

std::unique_ptr<Shape> read_step_str(rust::Str data) {
  quietMessenger();
  STEPControl_Reader rd;
  std::istringstream is(std::string(data.data(), data.size()));
  return finishStepRead(rd, rd.ReadStream("memory.step", is));
}

namespace {
void transferStep(STEPControl_Writer &w, const Shape &s) {
  quietMessenger();
  requireShape(s, "write_step");
  Interface_Static::SetCVal("write.step.unit", "MM");
  Interface_Static::SetCVal("write.step.schema", "AP214IS");
  if (w.Transfer(s.s, STEPControl_AsIs) != IFSelect_RetDone) fail("STEP transfer failed");
}
} // namespace

void write_step_file(const Shape &s, rust::Str path) {
  STEPControl_Writer w;
  transferStep(w, s);
  std::string p(path);
  if (w.Write(p.c_str()) != IFSelect_RetDone) fail("cannot write STEP file " + p);
}

rust::String write_step_str(const Shape &s) {
  STEPControl_Writer w;
  transferStep(w, s);
  std::ostringstream os;
  if (w.WriteStream(os) != IFSelect_RetDone) fail("cannot write STEP stream");
  return rust::String(os.str());
}

void write_stl_file(const Shape &s, rust::Str path, double lin_defl, bool ascii) {
  requireShape(s, "write_stl");
  {
    std::lock_guard<std::mutex> lock(meshMutex());
    BRepMesh_IncrementalMesh mesher(s.s, lin_defl, Standard_False, 0.5, Standard_False);
    if (!mesher.IsDone()) fail("meshing failed");
  }
  StlAPI_Writer w;
  w.ASCIIMode() = ascii;
  std::string p(path);
  if (!w.Write(s.s, p.c_str())) fail("cannot write STL file " + p);
}

} // namespace cw
