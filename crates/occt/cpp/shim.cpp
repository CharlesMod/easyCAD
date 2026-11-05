// Narrow OpenCascade shim for easyCAD. Every entry point either returns a valid
// result or throws (std::runtime_error / Standard_Failure); `shim.h` converts
// exceptions into Rust `cxx::Exception`s.

#include "shim.h"
#include "occt/src/ffi.rs.h"

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cmath>
#include <csignal>
#include <cstring>
#include <cstdlib>
#include <map>
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
#include <BRepOffsetAPI_ThruSections.hxx>
#include <BRepBuilderAPI_Sewing.hxx>
#include <BRepLib.hxx>
#include <ShapeAnalysis_Curve.hxx>
#include <TopoDS_Vertex.hxx>
#include <TopoDS_Solid.hxx>
#include <BRepAdaptor_Surface.hxx>
#include <BRepAlgoAPI_Common.hxx>
#include <BRepAlgoAPI_Cut.hxx>
#include <BRepAlgoAPI_Fuse.hxx>
#include <BRepBndLib.hxx>
#include <BRepBuilderAPI_Copy.hxx>
#include <BRepBuilderAPI_MakeEdge.hxx>
#include <BRepBuilderAPI_MakeFace.hxx>
#include <BRepBuilderAPI_MakeWire.hxx>
#include <BRepBuilderAPI_Transform.hxx>
#include <BRepCheck_Analyzer.hxx>
#include <BRepFilletAPI_MakeChamfer.hxx>
#include <BRepFilletAPI_MakeFillet.hxx>
#include <BRepFilletAPI_MakeFillet2d.hxx>
#include <BRepGProp.hxx>
#include <BRepTools_WireExplorer.hxx>
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
#include <STEPCAFControl_Reader.hxx>
#include <STEPCAFControl_Writer.hxx>
#include <STEPControl_Reader.hxx>
#include <TDF_ChildIterator.hxx>
#include <TDF_Label.hxx>
#include <TDF_LabelSequence.hxx>
#include <TDataStd_Name.hxx>
#include <TDF_Tool.hxx>
#include <TDocStd_Document.hxx>
#include <XCAFApp_Application.hxx>
#include <XCAFDoc_ColorTool.hxx>
#include <XCAFDoc_DocumentTool.hxx>
#include <XCAFDoc_ShapeTool.hxx>
#include <Quantity_Color.hxx>
#include <TCollection_AsciiString.hxx>
#include <TCollection_ExtendedString.hxx>
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

// OCCT's type registry (a function-local static in Standard_Type.cxx) must be
// built before any static that holds type handles, so that it is destroyed after
// them. OCCT touches it from a static in Standard_Type.cxx for this, but ld64 runs
// static initializers in link order and TKernel links last, after e.g.
// StepAP214_Protocol.cxx's `types` map: at exit that map released its handles into
// an already destroyed registry and every quit crashed (SIGSEGV in
// ~Standard_Type, from NSApplication terminate: -> exit). This shim links before
// every OCCT library, so this initializer runs first.
const Handle(Standard_Type) gTypeRegistryFirst = STANDARD_TYPE(Standard_Transient);

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

// Booleans run serially: on enclosure-sized models OCCT's parallel boolean costs
// about 3x the CPU for no gain in wall time (and loses when the machine is busy);
// independent booleans are run concurrently by the callers instead (the joint's
// two halves). CW_PARALLEL_BOP=1 turns the parallel boolean back on to compare,
// CW_NO_OBB=1 the oriented-box prefilter off. Meshing stays parallel (it scales):
// CW_SERIAL_MESH=1 to compare.
bool parallelBop() {
  static const bool on = std::getenv("CW_PARALLEL_BOP") != nullptr;
  return on;
}
bool useObb() {
  static const bool on = std::getenv("CW_NO_OBB") == nullptr;
  return on;
}
bool parallelMesh() {
  static const bool on = std::getenv("CW_SERIAL_MESH") == nullptr;
  return on;
}

// ---- concurrency -----------------------------------------------------------
//
// A `Shape` may be used from several threads at once (Rust `Send + Sync`), so no
// entry point may write into an input's topology. OCCT stores mutable caches inside
// the shared TShapes: triangulations and edge polygons (BRepMesh, BRepTools::Clean),
// and tolerances/pcurves that ShapeFix, the offset/thick-solid algorithms and
// sometimes the fillet builder update in place. Readers of those caches include
// BRepBndLib::Add (used by every boolean's BOPDS_DS::InitShapes), HLR PolyAlgo and the
// STL writer. Meshing one body while another thread booleaned it was a
// use-after-free on the replaced Poly_Triangulation (reproduced as SIGSEGV in
// tests/concurrency.rs) and, single-threaded, made boolean bounding boxes depend on
// whether the body had been displayed. So every operation that can mutate topology
// runs on a private copy of its input (new TShapes, shared immutable geometry).
TopoDS_Shape privateCopy(const TopoDS_Shape &s) {
  BRepBuilderAPI_Copy cp(s, /*copyGeom=*/Standard_False, /*copyMesh=*/Standard_False);
  if (!cp.IsDone()) fail("cannot copy shape");
  return cp.Shape();
}

// STEP/XCAF translation keeps process-global state (Interface_Static parameters with
// no locking, the XCAF application's document table, protocol registries). All STEP
// entry points run under this lock. The nested OCC_CATCH_SIGNALS makes a converted
// kernel signal unwind through the lock_guard (a longjmp to the outer handler in
// rust::behavior::trycatch would skip it and leave the mutex locked forever).
std::mutex &stepMutex() {
  static std::mutex m;
  return m;
}

template <class F> auto withStepLock(F &&f) -> decltype(f()) {
  std::lock_guard<std::mutex> lock(stepMutex());
  OCC_CATCH_SIGNALS
  return f();
}

} // namespace

// ---- one-time process setup (called from shim.h before the first kernel call) --

namespace {
struct sigaction gPrevSegv, gPrevBus, gOccSegv, gOccBus;

void forwardSignal(const struct sigaction &h, int sig, siginfo_t *info, void *ctx) {
  if (h.sa_flags & SA_SIGINFO) {
    h.sa_sigaction(sig, info, ctx);
  } else if (h.sa_handler != SIG_DFL && h.sa_handler != SIG_IGN) {
    h.sa_handler(sig);
  } else {
    // default action: reinstate it and return; the fault re-executes and kills the
    // process with the right signal (core dump), as without OCCT.
    struct sigaction dfl;
    std::memset(&dfl, 0, sizeof dfl);
    dfl.sa_handler = SIG_DFL;
    sigemptyset(&dfl.sa_mask);
    sigaction(sig, &dfl, nullptr);
  }
}

// OCCT's handler converts a fault into a C++ exception by longjmp-ing to the
// innermost OCC_CATCH_SIGNALS of the *faulting thread*; with none (any non-kernel
// thread: Rust code, GPU drivers, the GUI) it prints "*** Abort ***" and calls exit()
// from inside the signal handler, which runs static destructors under the other
// threads' feet. Route faults outside kernel calls to the handler that was installed
// before OCCT's (Rust's stack-overflow reporter, or the default action) instead.
// SA_ONSTACK lets both run on the thread's alternate signal stack, so a stack
// overflow inside the kernel is converted too instead of killing the process.
void chainedFault(int sig, siginfo_t *info, void *ctx) {
  const bool inKernel = Standard_ErrorHandler::IsInTryBlock();
  if (sig == SIGSEGV) forwardSignal(inKernel ? gOccSegv : gPrevSegv, sig, info, ctx);
  else forwardSignal(inKernel ? gOccBus : gPrevBus, sig, info, ctx);
}
} // namespace

void initKernelOnce() {
  sigaction(SIGSEGV, nullptr, &gPrevSegv);
  sigaction(SIGBUS, nullptr, &gPrevBus);
  OSD::SetSignal(OSD_SignalMode_Set, Standard_False);
  sigaction(SIGSEGV, nullptr, &gOccSegv);
  sigaction(SIGBUS, nullptr, &gOccBus);
  struct sigaction ch;
  std::memset(&ch, 0, sizeof ch);
  ch.sa_sigaction = chainedFault;
  ch.sa_flags = SA_SIGINFO | SA_ONSTACK | SA_NODEFER;
  sigemptyset(&ch.sa_mask);
  sigaction(SIGSEGV, &ch, nullptr);
  sigaction(SIGBUS, &ch, nullptr);
  // OCCT prints through a shared messenger; silence it once, before any algorithm can
  // be iterating its printer list on another thread.
  Handle(Message_Messenger) msgr = Message::DefaultMessenger();
  if (!msgr.IsNull()) msgr->ChangePrinters().Clear();
}

namespace {

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
  alg->SetUseOBB(useObb());
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

uint32_t child_count(const Shape &s) {
  uint32_t n = 0;
  if (s.s.IsNull()) return 0;
  for (TopoDS_Iterator it(s.s); it.More(); it.Next()) ++n;
  return n;
}

std::unique_ptr<Shape> child_at(const Shape &s, uint32_t i) {
  requireShape(s, "child_at");
  uint32_t n = 0;
  for (TopoDS_Iterator it(s.s); it.More(); it.Next(), ++n)
    if (n == i) return wrap(it.Value());
  fail("child_at: index out of range");
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
  f.SetUseOBB(useObb());
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
  return wrap(simplify(doUnify(privateCopy(s.s))));
}

std::unique_ptr<Shape> heal(const Shape &s) {
  requireShape(s, "heal");
  Handle(ShapeFix_Shape) fix = new ShapeFix_Shape(privateCopy(s.s));
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

namespace {
// A dress-up that eats through a thin feature (a chamfer wider than its hole, a
// fillet bigger than a slot) can come back as a valid *shell*: BRepCheck accepts
// it, but the body is gone. Refuse it, so the caller's ladder reduces the size.
void requireSolids(const TopoDS_Shape &in, const TopoDS_Shape &out, const char *what) {
  int a = 0, b = 0;
  for (TopExp_Explorer ex(in, TopAbs_SOLID); ex.More(); ex.Next()) ++a;
  for (TopExp_Explorer ex(out, TopAbs_SOLID); ex.More(); ex.Next()) ++b;
  if (b < a) fail(std::string(what) + " cut through the body (the size is larger than a hole or wall it touches)");
}
} // namespace

std::unique_ptr<Shape> fillet(const Shape &s, rust::Slice<const uint32_t> edges, rust::Slice<const double> radii) {
  requireShape(s, "fillet");
  if (edges.empty()) fail("fillet: no edges selected");
  if (radii.size() != edges.size()) fail("fillet: one radius per edge required");
  TopoDS_Shape in = privateCopy(s.s); // same MapShapes order as s
  auto m = mapOf(in, TopAbs_EDGE);
  BRepFilletAPI_MakeFillet mf(in);
  for (size_t k = 0; k < edges.size(); ++k) {
    uint32_t i = edges[k];
    if (i >= static_cast<uint32_t>(m.Extent())) fail("fillet: edge index out of range");
    if (radii[k] <= 0) fail("fillet radius must be positive");
    mf.Add(radii[k], TopoDS::Edge(m.FindKey(static_cast<int>(i) + 1)));
  }
  mf.Build();
  if (!mf.IsDone()) fail("fillet failed (radius too large for the adjacent faces?)");
  TopoDS_Shape r = healIfInvalid(mf.Shape(), "fillet");
  requireSolids(in, r, "fillet");
  return wrap(simplify(r));
}

std::unique_ptr<Shape> chamfer(const Shape &s, rust::Slice<const uint32_t> edges, rust::Slice<const double> dists) {
  requireShape(s, "chamfer");
  if (edges.empty()) fail("chamfer: no edges selected");
  if (dists.size() != edges.size()) fail("chamfer: one distance per edge required");
  TopoDS_Shape in = privateCopy(s.s);
  auto m = mapOf(in, TopAbs_EDGE);
  BRepFilletAPI_MakeChamfer mc(in);
  for (size_t k = 0; k < edges.size(); ++k) {
    uint32_t i = edges[k];
    if (i >= static_cast<uint32_t>(m.Extent())) fail("chamfer: edge index out of range");
    if (dists[k] <= 0) fail("chamfer distance must be positive");
    mc.Add(dists[k], TopoDS::Edge(m.FindKey(static_cast<int>(i) + 1)));
  }
  mc.Build();
  if (!mc.IsDone()) fail("chamfer failed (distance too large?)");
  TopoDS_Shape r = healIfInvalid(mc.Shape(), "chamfer");
  requireSolids(in, r, "chamfer");
  return wrap(simplify(r));
}

namespace {
TopoDS_Shape doOffsetSolid(const TopoDS_Shape &in, double d);
} // namespace

std::unique_ptr<Shape> shell(const Shape &s, rust::Slice<const uint32_t> faces, double offset) {
  requireShape(s, "shell");
  if (std::abs(offset) < kTol) fail("shell thickness must be non-zero");
  TopoDS_Shape copy = privateCopy(s.s);
  auto m = mapOf(copy, TopAbs_FACE);
  TopTools_ListOfShape closing;
  for (uint32_t i : faces) {
    if (i >= static_cast<uint32_t>(m.Extent())) fail("shell: face index out of range");
    closing.Append(m.FindKey(static_cast<int>(i) + 1));
  }
  // The thick-solid algorithm wants a single solid.
  TopoDS_Shape input = simplify(copy);
  if (closing.IsEmpty()) {
    // Nothing to open: MakeThickSolid would return only the offset skin. Build the
    // closed hollow solid (outer skin + internal void) from a 3D offset instead.
    const bool prof = std::getenv("CW_PROF") != nullptr;
    auto t0 = std::chrono::steady_clock::now();
    auto lap = [&](const char *w) {
      if (prof)
        std::fprintf(stderr, "  shell %s: %.1f ms\n", w,
                     std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - t0).count());
    };
    GProp_GProps in_v, in_a;
    BRepGProp::VolumeProperties(input, in_v);
    BRepGProp::SurfaceProperties(input, in_a);
    const double V = in_v.Mass(), A = in_a.Mass(), t = std::abs(offset);
    // a believable result: one solid, two shells (skin + void), and (inward) a
    // void close to V - A·t (exact for flat walls; curvature changes it a little)
    auto plausible = [&](const TopoDS_Shape &r, std::string &why) -> bool {
      if (count_sub(Shape(r), TopAbs_SOLID) != 1) {
        why = "the walls are not one solid";
        return false;
      }
      if (count_sub(Shape(r), TopAbs_SHELL) != 2) {
        why = "the result has no internal void";
        return false;
      }
      GProp_GProps a;
      BRepGProp::VolumeProperties(r, a);
      if (offset < 0) {
        double removed = V - a.Mass();
        double est = V - A * t;
        if (!(a.Mass() > 0 && removed > 1e-9) || (est > 0 && (removed < 0.5 * est || removed > 1.5 * est + 1e-6))) {
          why = "the inner offset did not produce a believable cavity";
          return false;
        }
      }
      BRepCheck_Analyzer ana(r);
      if (!ana.IsValid()) {
        why = "the result is invalid";
        return false;
      }
      return true;
    };
    lap("input props");
    std::string why;
    TopoDS_Shape r;
    try {
      TopoDS_Shape off = doOffsetSolid(privateCopy(input), offset);
      lap("offset solid");
      // inward: the void is the inner offset's skin, reversed, as a second shell of
      // the input solid (no boolean needed when the skins do not touch; checked)
      if (offset < 0) {
        try {
          TopoDS_Solid hollow;
          BRep_Builder bld;
          bld.MakeSolid(hollow);
          int outer = 0, inner = 0;
          for (TopExp_Explorer ex(input, TopAbs_SHELL); ex.More(); ex.Next()) {
            bld.Add(hollow, ex.Current());
            ++outer;
          }
          for (TopExp_Explorer ex(off, TopAbs_SHELL); ex.More(); ex.Next()) {
            bld.Add(hollow, ex.Current().Reversed());
            ++inner;
          }
          if (outer == 1 && inner == 1) {
            std::string why0;
            if (plausible(hollow, why0)) {
              lap("direct void");
              return wrap(hollow);
            }
          }
        } catch (const Standard_Failure &) {
        }
      }
      r = offset < 0 ? doBoolean(input, off, 1u) : doBoolean(off, input, 1u);
      lap("boolean");
      r = simplify(r);
      lap("simplify");
    } catch (const Standard_Failure &e) {
      why = std::string("the ") + (offset < 0 ? "inner" : "outer") + " offset failed (thickness larger than a convex radius or a thin feature?): " + e.GetMessageString();
    } catch (const std::exception &e) {
      why = std::string("the ") + (offset < 0 ? "inner" : "outer") + " offset failed (thickness larger than a convex radius or a thin feature?): " + e.what();
    }
    if (!r.IsNull() && plausible(r, why)) {
      lap("checks");
      return wrap(r);
    }
    // Fallback (inward only): shell with the largest planar face open (the robust
    // thick-solid path), then close it again with that face's slab of the input.
    if (offset < 0) {
      TopoDS_Face best;
      double best_a = 0;
      for (TopExp_Explorer ex(input, TopAbs_FACE); ex.More(); ex.Next()) {
        TopoDS_Face f = TopoDS::Face(ex.Current());
        if (BRepAdaptor_Surface(f).GetType() != GeomAbs_Plane) continue;
        GProp_GProps g;
        BRepGProp::SurfaceProperties(f, g);
        if (g.Mass() > best_a) {
          best_a = g.Mass();
          best = f;
        }
      }
      if (!best.IsNull()) {
        try {
          OCC_CATCH_SIGNALS
          TopTools_ListOfShape open;
          open.Append(best);
          BRepOffsetAPI_MakeThickSolid mk;
          mk.MakeThickSolidByJoin(input, open, offset, 1e-3, BRepOffset_Skin, Standard_False, Standard_False, GeomAbs_Arc);
          mk.Build();
          if (mk.IsDone()) {
            TopoDS_Shape cup = mk.Shape();
            gp_Pln pl = BRepAdaptor_Surface(best).Plane();
            gp_Dir nn = pl.Axis().Direction();
            if (best.Orientation() == TopAbs_REVERSED) nn.Reverse();
            // slab: the open face pushed t into the solid, clipped to the input
            TopoDS_Shape slab = BRepPrimAPI_MakePrism(best, gp_Vec(nn) * (-t)).Shape();
            slab = doBoolean(input, slab, 2u);
            TopoDS_Shape c = simplify(doBoolean(cup, slab, 0u));
            std::string why2;
            if (plausible(c, why2)) return wrap(c);
            why += "; open-and-close fallback: " + why2;
          } else {
            why += "; open-and-close fallback: the thick solid failed";
          }
        } catch (const Standard_Failure &e) {
          why += std::string("; open-and-close fallback: ") + e.GetMessageString();
        } catch (const std::exception &e) {
          why += std::string("; open-and-close fallback: ") + e.what();
        }
      }
    }
    fail("shell with no open faces: " + why);
  }
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
  TopoDS_Shape in = privateCopy(s.s);
  auto m = mapOf(in, TopAbs_FACE);
  BRepOffsetAPI_DraftAngle dr(in);
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
// MakeOffset's arc joins leave zero-length arcs where two edges meet almost
// tangentially (a line ending 1e-7 off its arc). They are harmless in the region,
// but a drafted extrusion of it turns them into cones whose radius goes negative.
// Rebuild such a wire without them (the neighbours' ends coincide within tolerance).
TopoDS_Wire dropTinyEdges(const TopoDS_Wire &w) {
  std::vector<TopoDS_Edge> keep;
  bool tiny = false;
  for (BRepTools_WireExplorer ex(w); ex.More(); ex.Next()) {
    GProp_GProps g;
    BRepGProp::LinearProperties(ex.Current(), g);
    if (g.Mass() < 1e-4) {
      tiny = true;
      continue;
    }
    keep.push_back(ex.Current());
  }
  if (!tiny || keep.empty()) return w;
  try {
    OCC_CATCH_SIGNALS
    return wireFrom(keep);
  } catch (...) {
    return w;
  }
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
  for (auto &f : facesOf(privateCopy(s.s))) {
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
    // MAT2d (used by MakeOffset) is fragile on tiny edges, split collinear edges,
    // near-tangent loops and sharp corners. Fallback ladder (first success wins):
    // cleaned face, original face, each loop on its own (holes offset the other
    // way, then cut out), intersection joins, and a tiny corner radius bump.
    auto attempt_join = [&](const TopoDS_Face &face, double dd, GeomAbs_JoinType join) -> TopoDS_Shape {
      try {
        OCC_CATCH_SIGNALS
        BRepOffsetAPI_MakeOffset mo(face, join, Standard_False);
        mo.Perform(dd);
        if (!mo.IsDone()) return TopoDS_Shape();
        TopoDS_Shape r = mo.Shape();
        if (r.IsNull()) return TopoDS_Shape();
        bool closed = false;
        for (TopExp_Explorer ex(r, TopAbs_WIRE); ex.More(); ex.Next())
          if (BRep_Tool::IsClosed(TopoDS::Wire(ex.Current()))) closed = true;
        return closed ? r : TopoDS_Shape();
      } catch (const Standard_Failure &) {
        return TopoDS_Shape();
      }
    };
    auto attempt = [&](const TopoDS_Face &face) -> TopoDS_Shape { return attempt_join(face, d, GeomAbs_Arc); };
    // one face per loop of `face`, oriented with the plane normal
    auto loopFace = [&](const TopoDS_Wire &w) -> TopoDS_Face {
      BRepBuilderAPI_MakeFace mf(pln, w, Standard_True);
      if (!mf.IsDone()) return TopoDS_Face();
      Handle(ShapeFix_Face) sff = new ShapeFix_Face(mf.Face());
      sff->FixOrientationMode() = 1;
      sff->Perform();
      TopoDS_Face lf = sff->Face();
      gp_Dir ln = planeOf(lf).Axis().Direction();
      if (lf.Orientation() == TopAbs_REVERSED) ln.Reverse();
      if (ln.Dot(normal) < 0) lf.Reverse();
      return lf;
    };
    auto facesOfWires = [&](const TopoDS_Shape &r) -> TopoDS_Shape {
      std::vector<TopoDS_Wire> ws;
      for (TopExp_Explorer ex(r, TopAbs_WIRE); ex.More(); ex.Next()) {
        TopoDS_Wire w = TopoDS::Wire(ex.Current());
        if (BRep_Tool::IsClosed(w)) ws.push_back(w);
      }
      if (ws.empty()) return TopoDS_Shape();
      return facesFromWires(pln, ws, normal);
    };
    // offset the outer loop by d and each hole loop by -d, then cut the holes out
    auto per_loop = [&](const TopoDS_Face &face, GeomAbs_JoinType join) -> TopoDS_Shape {
      try {
        OCC_CATCH_SIGNALS
        TopoDS_Wire outer = BRepTools::OuterWire(face);
        TopoDS_Face of = loopFace(outer);
        if (of.IsNull()) return TopoDS_Shape();
        TopoDS_Shape o = attempt_join(of, d, join);
        if (o.IsNull()) return TopoDS_Shape();
        TopoDS_Shape acc = facesOfWires(o);
        if (acc.IsNull()) return TopoDS_Shape();
        for (TopExp_Explorer ex(face, TopAbs_WIRE); ex.More(); ex.Next()) {
          TopoDS_Wire w = TopoDS::Wire(ex.Current());
          if (w.IsSame(outer)) continue;
          TopoDS_Face hf = loopFace(w);
          if (hf.IsNull()) return TopoDS_Shape();
          TopoDS_Shape h = attempt_join(hf, -d, join);
          if (h.IsNull()) {
            // a hole may legitimately vanish when the region grows past it
            Bnd_Box hb;
            BRepBndLib::Add(hf, hb);
            double x0, y0, z0, x1, y1, z1;
            hb.Get(x0, y0, z0, x1, y1, z1);
            double dims[3] = {x1 - x0, y1 - y0, z1 - z0};
            std::sort(dims, dims + 3);
            if (d > 0 && dims[1] < 2.0 * d + 1e-6) continue;
            return TopoDS_Shape();
          }
          TopoDS_Shape hfaces = facesOfWires(h);
          if (hfaces.IsNull()) continue;
          acc = doBoolean(acc, hfaces, 1u);
        }
        return acc;
      } catch (const Standard_Failure &) {
        return TopoDS_Shape();
      }
    };
    // round sharp corners by a tiny radius (a degenerate corner trips MAT2d)
    auto bumped = [&](const TopoDS_Face &face) -> TopoDS_Face {
      double r = std::min(0.05, std::abs(d) / 4.0);
      if (r < 1e-4) return TopoDS_Face();
      try {
        OCC_CATCH_SIGNALS
        BRepFilletAPI_MakeFillet2d mf(face);
        TopTools_IndexedDataMapOfShapeListOfShape ve;
        TopExp::MapShapesAndAncestors(face, TopAbs_VERTEX, TopAbs_EDGE, ve);
        int added = 0;
        for (int i = 1; i <= ve.Extent(); ++i) {
          if (ve(i).Extent() != 2) continue;
          try {
            mf.AddFillet(TopoDS::Vertex(ve.FindKey(i)), r);
            if (mf.Status() == ChFi2d_IsDone) ++added;
          } catch (const Standard_Failure &) {
          }
        }
        if (added == 0) return TopoDS_Face();
        mf.Build();
        if (!mf.IsDone()) return TopoDS_Face();
        TopExp_Explorer fx(mf.Shape(), TopAbs_FACE);
        return fx.More() ? TopoDS::Face(fx.Current()) : TopoDS_Face();
      } catch (const Standard_Failure &) {
        return TopoDS_Face();
      }
    };
    TopoDS_Face clean = f;
    try {
      TopoDS_Shape u = doUnify(f);
      TopExp_Explorer fx(u, TopAbs_FACE);
      if (fx.More()) clean = TopoDS::Face(fx.Current());
      Handle(ShapeFix_Face) sff = new ShapeFix_Face(clean);
      sff->FixWireTool()->FixSmallMode() = 1;
      sff->SetPrecision(1e-5);
      sff->Perform();
      clean = sff->Face();
    } catch (...) {
      clean = f;
    }
    TopoDS_Shape res = attempt(clean);
    if (res.IsNull()) res = attempt(f);
    if (res.IsNull()) res = per_loop(clean, GeomAbs_Arc);
    if (res.IsNull()) res = attempt_join(clean, d, GeomAbs_Intersection);
    if (res.IsNull()) res = per_loop(clean, GeomAbs_Intersection);
    if (res.IsNull()) {
      TopoDS_Face b = bumped(clean);
      if (!b.IsNull()) {
        res = attempt(b);
        if (res.IsNull()) res = per_loop(b, GeomAbs_Arc);
      }
    }
    if (res.IsNull()) fail("2D offset failed");
    for (TopExp_Explorer ex(res, TopAbs_WIRE); ex.More(); ex.Next()) {
      TopoDS_Wire w = TopoDS::Wire(ex.Current());
      if (BRep_Tool::IsClosed(w)) wires.push_back(dropTinyEdges(w));
    }
  }
  if (first) fail("offset_face: no planar face");
  if (wires.empty()) fail("offset region vanished");
  return wrap(facesFromWires(pln, wires, normal));
}

std::unique_ptr<Shape> taper_prism(const Shape &s, V3 v, double inset) {
  requireShape(s, "taper");
  gp_Vec vec = Vv(v);
  if (vec.Magnitude() < kTol) fail("taper height must be non-zero");
  if (inset <= 0) fail("taper inset must be positive");
  std::vector<TopoDS_Face> bottom = facesOf(s.s);
  gp_Trsf up;
  up.SetTranslation(vec);
  std::vector<TopoDS_Face> top = facesOf(BRepBuilderAPI_Transform(offset_face(s, -inset)->s, up, Standard_True).Shape());
  if (bottom.size() != 1 || top.size() != 1) fail("taper: the region splits or vanishes at that inset");
  auto loops = [](const TopoDS_Face &f) {
    TopoDS_Wire outer = BRepTools::OuterWire(f);
    std::vector<TopoDS_Wire> holes;
    for (TopExp_Explorer ex(f, TopAbs_WIRE); ex.More(); ex.Next())
      if (!ex.Current().IsSame(outer)) holes.push_back(TopoDS::Wire(ex.Current()));
    return std::make_pair(outer, holes);
  };
  auto [bo, bh] = loops(bottom[0]);
  auto [to, th] = loops(top[0]);
  if (bh.size() != th.size()) fail("taper: a hole vanishes at that inset");
  auto centre = [](const TopoDS_Shape &w) {
    Bnd_Box b;
    BRepBndLib::Add(w, b);
    double x0, y0, z0, x1, y1, z1;
    b.Get(x0, y0, z0, x1, y1, z1);
    return gp_Pnt((x0 + x1) / 2, (y0 + y1) / 2, (z0 + z1) / 2);
  };
  auto nEdges = [](const TopoDS_Wire &w) {
    int n = 0;
    for (BRepTools_WireExplorer ex(w); ex.More(); ex.Next()) ++n;
    return n;
  };
  // ruled side faces of a loop pair; sewn with both caps into one closed shell
  // (no boolean: the loops never cross)
  BRepBuilderAPI_Sewing sew(1e-6);
  sew.Add(bottom[0]);
  sew.Add(top[0]);
  auto loft = [&](const TopoDS_Wire &a, const TopoDS_Wire &b) {
    if (nEdges(a) != nEdges(b)) fail("taper: a corner changes shape at that inset");
    BRepOffsetAPI_ThruSections ts(Standard_False, Standard_True);
    ts.AddWire(a);
    ts.AddWire(b);
    ts.Build();
    if (!ts.IsDone()) fail("taper: ruled loft failed");
    for (TopExp_Explorer ex(ts.Shape(), TopAbs_FACE); ex.More(); ex.Next()) sew.Add(ex.Current());
  };
  loft(bo, to);
  std::vector<bool> used(th.size(), false);
  for (const auto &h : bh) {
    gp_Pnt c = centre(h);
    int best = -1;
    double bd = 1e300;
    for (size_t j = 0; j < th.size(); ++j) {
      double d = centre(th[j]).Translated(-vec).Distance(c);
      if (!used[j] && d < bd) {
        bd = d;
        best = static_cast<int>(j);
      }
    }
    used[static_cast<size_t>(best)] = true;
    loft(h, th[static_cast<size_t>(best)]);
  }
  sew.Perform();
  TopoDS_Shape sewn = sew.SewedShape();
  TopExp_Explorer sx(sewn, TopAbs_SHELL);
  if (!sx.More()) fail("taper: the faces do not close");
  TopoDS_Shell shell = TopoDS::Shell(sx.Current());
  sx.Next();
  if (sx.More() || !BRep_Tool::IsClosed(shell)) fail("taper: the faces do not close");
  TopoDS_Solid solid;
  BRep_Builder bb;
  bb.MakeSolid(solid);
  bb.Add(solid, shell);
  BRepLib::OrientClosedSolid(solid);
  TopoDS_Shape r = solid;
  BRepCheck_Analyzer an(r);
  if (!an.IsValid()) fail("taper: the result is not a valid solid");
  return wrap(r);
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

namespace {
// 3D skin offset of a solid as a solid (the caller passes a private copy).
TopoDS_Shape doOffsetSolid(const TopoDS_Shape &in, double d) {
  BRepOffsetAPI_MakeOffsetShape mo;
  mo.PerformByJoin(in, d, 1e-4, BRepOffset_Skin, Standard_False, Standard_False, GeomAbs_Arc);
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
  return simplify(healIfInvalid(r, "offset"));
}
} // namespace

std::unique_ptr<Shape> offset_shape(const Shape &s, double d) {
  requireShape(s, "offset_shape");
  return wrap(doOffsetSolid(privateCopy(s.s), d));
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
  // Mesh a private copy: the triangulation lives inside the TShapes, which other
  // threads may be reading (booleans, HLR, STL) or meshing at another density. A fresh
  // copy also means each quality level gets its own density (BRepMesh never coarsens).
  TopoDS_Shape m = privateCopy(s.s);
  BRepMesh_IncrementalMesh mesher(m, lin_defl, Standard_False, ang_defl_rad, parallelMesh());
  if (!mesher.IsDone()) fail("meshing failed");
  MeshData md;
  auto fm = mapOf(m, TopAbs_FACE);
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
  auto em = mapOf(m, TopAbs_EDGE);
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
      algo->Add(privateCopy(s.s));
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
    TopoDS_Shape m = privateCopy(s.s); // PolyAlgo needs a triangulation: mesh a copy
    BRepMesh_IncrementalMesh mesher(m, defl, Standard_False, 0.2, Standard_False);
    Handle(HLRBRep_PolyAlgo) algo = new HLRBRep_PolyAlgo(m);
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
  // The optimal box evaluates surface derivatives, which throws on degenerate
  // offset surfaces (e.g. an inward shell of a fillet blend). Fall back to the
  // geometric box, then to the box of a fine private mesh.
  bool done = false;
  try {
    OCC_CATCH_SIGNALS
    BRepBndLib::AddOptimal(s.s, bb, Standard_False, Standard_False);
    done = true;
  } catch (const Standard_Failure &) {
  }
  if (!done) {
    bb.SetVoid();
    try {
      OCC_CATCH_SIGNALS
      BRepBndLib::Add(s.s, bb, Standard_False);
      done = true;
    } catch (const Standard_Failure &) {
    }
  }
  if (!done) {
    bb.SetVoid();
    TopoDS_Shape copy = privateCopy(s.s);
    BRepMesh_IncrementalMesh(copy, 0.01, Standard_False, 0.2);
    BRepBndLib::Add(copy, bb, Standard_True);
  }
  if (bb.IsVoid()) fail("bounding box of empty shape");
  double x0, y0, z0, x1, y1, z1;
  bb.Get(x0, y0, z0, x1, y1, z1);
  BBox r{};
  r.min = mk(x0, y0, z0);
  r.max = mk(x1, y1, z1);
  return r;
}

rust::Vec<double> extent_along(const Shape &s, V3 origin, V3 dir) {
  requireShape(s, "extent_along");
  gp_Pnt o = P(origin);
  gp_Dir d = D(dir, "extent direction");
  double lo = 1e300, hi = -1e300;
  auto add = [&](const gp_Pnt &p) {
    double t = gp_Vec(o, p).Dot(gp_Vec(d));
    lo = std::min(lo, t);
    hi = std::max(hi, t);
  };
  for (TopExp_Explorer ex(s.s, TopAbs_VERTEX); ex.More(); ex.Next()) add(BRep_Tool::Pnt(TopoDS::Vertex(ex.Current())));
  TopTools_IndexedMapOfShape edges;
  TopExp::MapShapes(s.s, TopAbs_EDGE, edges);
  for (int i = 1; i <= edges.Extent(); ++i) {
    const TopoDS_Edge &e = TopoDS::Edge(edges(i));
    if (BRep_Tool::Degenerated(e)) continue;
    try {
      BRepAdaptor_Curve c(e);
      double a = c.FirstParameter(), b = c.LastParameter();
      for (int k = 1; k < 32; ++k) add(c.Value(a + (b - a) * k / 32.0));
    } catch (const Standard_Failure &) {
    }
  }
  if (lo > hi) fail("extent of an empty shape");
  rust::Vec<double> out;
  out.push_back(lo);
  out.push_back(hi);
  return out;
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

namespace {
std::string labelName(const TDF_Label &l) {
  Handle(TDataStd_Name) n;
  if (!l.IsNull() && l.FindAttribute(TDataStd_Name::GetID(), n)) {
    TCollection_AsciiString a(n->Get());
    return a.ToCString();
  }
  return "";
}

bool labelColor(const Handle(XCAFDoc_ColorTool) &ct, const TDF_Label &l, Quantity_Color &c) {
  return !l.IsNull() && (ct->GetColor(l, XCAFDoc_ColorSurf, c) || ct->GetColor(l, XCAFDoc_ColorGen, c));
}

struct XcafCollect {
  Handle(XCAFDoc_ShapeTool) st;
  Handle(XCAFDoc_ColorTool) ct;
  BRep_Builder bb;
  TopoDS_Compound comp;
  rust::Vec<XcafPart> *parts;
  // faces (placed) with a colour of their own (AP214 STYLED_ITEM on the face)
  std::vector<std::pair<TopoDS_Shape, Quantity_Color>> faceColors;

  // `label`'s own shape `own` placed by `loc`: one part per solid
  void leaf(const TDF_Label &label, const TopoDS_Shape &own, const TopLoc_Location &loc, const std::string &name, bool hasC,
            const Quantity_Color &c) {
    // per-face colours are sub-shape labels of the part, in its own frame
    for (TopExp_Explorer fx(own, TopAbs_FACE); fx.More(); fx.Next()) {
      TDF_Label fl;
      Quantity_Color fc;
      if (st->FindSubShape(label, fx.Current(), fl) && labelColor(ct, fl, fc)) faceColors.emplace_back(fx.Current().Moved(loc), fc);
    }
    TopoDS_Shape shape = own.Moved(loc);
    int k = 0;
    for (TopExp_Explorer ex(shape, TopAbs_SOLID); ex.More(); ex.Next()) {
      Quantity_Color sc = c;
      bool has = hasC;
      Quantity_Color own;
      if (ct->GetColor(ex.Current(), XCAFDoc_ColorSurf, own) || ct->GetColor(ex.Current(), XCAFDoc_ColorGen, own)) {
        sc = own;
        has = true;
      }
      bb.Add(comp, ex.Current());
      XcafPart p;
      std::string nm = name.empty() ? std::string("Solid") : name;
      if (k > 0) nm += " (" + std::to_string(k + 1) + ")";
      p.name = rust::String(nm);
      double r, g, b;
      sc.Values(r, g, b, Quantity_TOC_sRGB);
      p.r = static_cast<float>(r);
      p.g = static_cast<float>(g);
      p.b = static_cast<float>(b);
      p.has_color = has;
      parts->push_back(p);
      ++k;
    }
  }

  void visit(const TDF_Label &label, const TopLoc_Location &loc, const std::string &instName, bool hasC, const Quantity_Color &c, int depth) {
    if (depth > 64) return;
    Quantity_Color myC = c;
    bool myHas = hasC;
    Quantity_Color lc;
    if (labelColor(ct, label, lc)) {
      myC = lc;
      myHas = true;
    }
    std::string name = instName.empty() ? labelName(label) : instName;
    if (XCAFDoc_ShapeTool::IsAssembly(label)) {
      TDF_LabelSequence comps;
      XCAFDoc_ShapeTool::GetComponents(label, comps);
      for (int i = 1; i <= comps.Length(); ++i) {
        TDF_Label comp = comps.Value(i);
        TDF_Label ref;
        if (!XCAFDoc_ShapeTool::GetReferredShape(comp, ref)) continue;
        Quantity_Color cc = myC;
        bool ch = myHas;
        Quantity_Color compC;
        if (labelColor(ct, comp, compC)) {
          cc = compC;
          ch = true;
        }
        std::string cn = labelName(comp);
        std::string rn = labelName(ref);
        // instance names like "=>[0:1:1:2]" are not useful; prefer the referred name
        std::string nm = (!rn.empty()) ? rn : cn;
        visit(ref, loc * XCAFDoc_ShapeTool::GetLocation(comp), nm, ch, cc, depth + 1);
      }
    } else {
      TopoDS_Shape s = XCAFDoc_ShapeTool::GetShape(label);
      if (s.IsNull()) return;
      leaf(label, s, loc, name, myHas, myC);
    }
  }
};
} // namespace

std::unique_ptr<Shape> read_step_xcaf(rust::Str data, rust::Vec<XcafPart> &parts, rust::Vec<float> &face_rgba) {
  return withStepLock([&] {
    Handle(XCAFApp_Application) app = XCAFApp_Application::GetApplication();
    Handle(TDocStd_Document) doc;
    app->NewDocument("MDTV-XCAF", doc);
    STEPCAFControl_Reader rd;
    rd.SetColorMode(Standard_True);
    rd.SetNameMode(Standard_True);
    rd.SetLayerMode(Standard_False);
    std::istringstream is(std::string(data.data(), data.size()));
    if (rd.ReadStream("memory.step", is) != IFSelect_RetDone) fail("STEP read failed (not a valid STEP file?)");
    if (!rd.Transfer(doc)) fail("STEP transfer failed");
    XcafCollect col;
    col.st = XCAFDoc_DocumentTool::ShapeTool(doc->Main());
    col.ct = XCAFDoc_DocumentTool::ColorTool(doc->Main());
    col.bb.MakeCompound(col.comp);
    col.parts = &parts;
    TDF_LabelSequence roots;
    col.st->GetFreeShapes(roots);
    Quantity_Color grey(0.7, 0.7, 0.7, Quantity_TOC_sRGB);
    for (int i = 1; i <= roots.Length(); ++i) col.visit(roots.Value(i), TopLoc_Location(), "", false, grey, 0);
    app->Close(doc);
    if (parts.empty()) fail("STEP file contains no solids");
    auto fm = mapOf(col.comp, TopAbs_FACE);
    std::vector<float> rgba(static_cast<size_t>(4 * fm.Extent()), 0.0f);
    for (const auto &fc : col.faceColors) {
      int idx = fm.FindIndex(fc.first);
      if (idx <= 0) continue;
      double r, g, b;
      fc.second.Values(r, g, b, Quantity_TOC_sRGB);
      float *o = &rgba[static_cast<size_t>(4 * (idx - 1))];
      o[0] = static_cast<float>(r);
      o[1] = static_cast<float>(g);
      o[2] = static_cast<float>(b);
      o[3] = 1.0f;
    }
    for (float v : rgba) face_rgba.push_back(v);
    return wrap(col.comp);
  });
}

rust::String write_step_xcaf(const Shape &c, rust::Slice<const XcafPart> parts, rust::Slice<const float> face_rgba) {
  return withStepLock([&] {
    requireShape(c, "write_step_xcaf");
    Handle(XCAFApp_Application) app = XCAFApp_Application::GetApplication();
    Handle(TDocStd_Document) doc;
    app->NewDocument("MDTV-XCAF", doc);
    Handle(XCAFDoc_ShapeTool) st = XCAFDoc_DocumentTool::ShapeTool(doc->Main());
    Handle(XCAFDoc_ColorTool) ct = XCAFDoc_DocumentTool::ColorTool(doc->Main());
    size_t i = 0;
    std::vector<TDF_Label> partLabels;
    for (TopoDS_Iterator it(c.s); it.More(); it.Next(), ++i) {
      TDF_Label l = st->AddShape(it.Value(), Standard_False);
      partLabels.push_back(l);
      if (i < parts.size()) {
        const XcafPart &p = parts[i];
        TDataStd_Name::Set(l, TCollection_ExtendedString(std::string(p.name).c_str()));
        if (p.has_color) ct->SetColor(l, Quantity_Color(p.r, p.g, p.b, Quantity_TOC_sRGB), XCAFDoc_ColorSurf);
      }
    }
    // face colours (4 floats per face of `c` in MapShapes order, alpha 0 = none),
    // written as STYLED_ITEMs on the faces
    if (!face_rgba.empty()) {
      auto fm = mapOf(c.s, TopAbs_FACE);
      if (face_rgba.size() != static_cast<size_t>(4 * fm.Extent())) fail("write_step_xcaf: face colours do not match the face count");
      size_t k = 0;
      for (TopoDS_Iterator it(c.s); it.More(); it.Next(), ++k) {
        const TDF_Label &l = partLabels[k];
        for (TopExp_Explorer fx(it.Value(), TopAbs_FACE); fx.More(); fx.Next()) {
          int idx = fm.FindIndex(fx.Current());
          if (idx <= 0) continue;
          const float *v = &face_rgba[static_cast<size_t>(4 * (idx - 1))];
          if (v[3] <= 0.0f) continue;
          TDF_Label fl = st->AddSubShape(l, fx.Current());
          if (!fl.IsNull()) ct->SetColor(fl, Quantity_Color(v[0], v[1], v[2], Quantity_TOC_sRGB), XCAFDoc_ColorSurf);
        }
      }
    }
    Interface_Static::SetCVal("write.step.unit", "MM");
    Interface_Static::SetCVal("write.step.schema", "AP214IS");
    STEPCAFControl_Writer w;
    w.SetColorMode(Standard_True);
    w.SetNameMode(Standard_True);
    if (!w.Transfer(doc, STEPControl_AsIs)) fail("STEP transfer failed");
    std::ostringstream os;
    if (w.WriteStream(os) != IFSelect_RetDone) fail("cannot write STEP stream");
    app->Close(doc);
    return rust::String(os.str());
  });
}

rust::Vec<uint32_t> face_owners(const Shape &c) {
  requireShape(c, "face_owners");
  auto fm = mapOf(c.s, TopAbs_FACE);
  std::vector<uint32_t> own(static_cast<size_t>(fm.Extent()), 0);
  uint32_t k = 0;
  for (TopoDS_Iterator it(c.s); it.More(); it.Next(), ++k) {
    for (TopExp_Explorer ex(it.Value(), TopAbs_FACE); ex.More(); ex.Next()) {
      int idx = fm.FindIndex(ex.Current());
      if (idx > 0) own[static_cast<size_t>(idx - 1)] = k;
    }
  }
  rust::Vec<uint32_t> out;
  for (auto v : own) out.push_back(v);
  return out;
}

namespace {
gp_Trsf trsfFrom(const double *m) {
  gp_Trsf t;
  t.SetValues(m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8], m[9], m[10], m[11]);
  return t;
}

void trsfTo(const gp_Trsf &t, rust::Vec<double> &out) {
  for (int r = 1; r <= 3; ++r)
    for (int c = 1; c <= 4; ++c) out.push_back(t.Value(r, c));
}

struct AsmRead {
  Handle(XCAFDoc_ShapeTool) st;
  Handle(XCAFDoc_ColorTool) ct;
  rust::Vec<AsmNode> *nodes;
  rust::Vec<AsmLink> *links;
  rust::Vec<double> *mats;
  BRep_Builder bb;
  TopoDS_Compound comp;
  int nshapes = 0;
  std::map<std::string, uint32_t> seen;

  uint32_t visit(const TDF_Label &label, int depth) {
    if (depth > 64) fail("STEP assembly is nested too deeply");
    TCollection_AsciiString entry;
    TDF_Tool::Entry(label, entry);
    std::string key = entry.ToCString();
    auto f = seen.find(key);
    if (f != seen.end()) return f->second;
    AsmNode n;
    n.name = rust::String(labelName(label));
    Quantity_Color c;
    n.has_color = labelColor(ct, label, c);
    if (!n.has_color) {
      TopoDS_Shape sh = XCAFDoc_ShapeTool::GetShape(label);
      n.has_color = !sh.IsNull() && (ct->GetColor(sh, XCAFDoc_ColorSurf, c) || ct->GetColor(sh, XCAFDoc_ColorGen, c));
    }
    if (n.has_color) {
      double r, g, b;
      c.Values(r, g, b, Quantity_TOC_sRGB);
      n.r = static_cast<float>(r);
      n.g = static_cast<float>(g);
      n.b = static_cast<float>(b);
    }
    n.shape = -1;
    const uint32_t me = static_cast<uint32_t>(nodes->size());
    nodes->push_back(n);
    seen[key] = me;
    if (XCAFDoc_ShapeTool::IsAssembly(label)) {
      TDF_LabelSequence comps;
      XCAFDoc_ShapeTool::GetComponents(label, comps);
      for (int i = 1; i <= comps.Length(); ++i) {
        TDF_Label cl = comps.Value(i);
        TDF_Label ref;
        if (!XCAFDoc_ShapeTool::GetReferredShape(cl, ref)) continue;
        uint32_t child = visit(ref, depth + 1);
        AsmLink l;
        l.parent = me;
        l.child = child;
        l.name = rust::String(labelName(cl));
        links->push_back(l);
        trsfTo(XCAFDoc_ShapeTool::GetLocation(cl).Transformation(), *mats);
      }
    } else {
      TopoDS_Shape sh = XCAFDoc_ShapeTool::GetShape(label);
      if (!sh.IsNull()) {
        bb.Add(comp, sh.Located(TopLoc_Location()));
        (*nodes)[me].shape = nshapes++;
      }
    }
    return me;
  }
};
} // namespace

rust::String write_step_asm(const Shape &shapes, rust::Slice<const AsmNode> nodes, rust::Slice<const AsmLink> links,
                            rust::Slice<const double> mats, rust::Slice<const uint32_t> roots) {
  return withStepLock([&] {
    requireShape(shapes, "write_step_asm");
    std::vector<TopoDS_Shape> kids;
    for (TopoDS_Iterator it(shapes.s); it.More(); it.Next()) kids.push_back(it.Value());
    const size_t n = nodes.size();
    if (n == 0) fail("STEP assembly: no products");
    if (mats.size() < links.size() * 12) fail("STEP assembly: one placement per instance required");
    for (uint32_t r : roots)
      if (r >= n) fail("STEP assembly: root index out of range");
    std::vector<bool> isAsm(n, false);
    for (const auto &l : links) {
      if (l.parent >= n || l.child >= n) fail("STEP assembly: instance index out of range");
      isAsm[l.parent] = true;
    }
    Handle(XCAFApp_Application) app = XCAFApp_Application::GetApplication();
    Handle(TDocStd_Document) doc;
    app->NewDocument("MDTV-XCAF", doc);
    Handle(XCAFDoc_ShapeTool) st = XCAFDoc_DocumentTool::ShapeTool(doc->Main());
    Handle(XCAFDoc_ColorTool) ct = XCAFDoc_DocumentTool::ColorTool(doc->Main());
    std::vector<TDF_Label> labels(n);
    for (size_t i = 0; i < n; ++i) {
      const AsmNode &nd = nodes[i];
      if (isAsm[i]) {
        labels[i] = st->NewShape();
      } else {
        if (nd.shape < 0 || static_cast<size_t>(nd.shape) >= kids.size()) fail("STEP assembly: part `" + std::string(nd.name) + "` has no shape");
        labels[i] = st->AddShape(kids[static_cast<size_t>(nd.shape)], Standard_False, Standard_False);
      }
      TDataStd_Name::Set(labels[i], TCollection_ExtendedString(std::string(nd.name).c_str(), Standard_True));
      if (nd.has_color) ct->SetColor(labels[i], Quantity_Color(nd.r, nd.g, nd.b, Quantity_TOC_sRGB), XCAFDoc_ColorSurf);
    }
    for (size_t k = 0; k < links.size(); ++k) {
      const AsmLink &l = links[k];
      TopLoc_Location loc(trsfFrom(mats.data() + 12 * k));
      TDF_Label c = st->AddComponent(labels[l.parent], labels[l.child], loc);
      if (c.IsNull()) fail("STEP assembly: cannot add instance `" + std::string(l.name) + "`");
      if (!l.name.empty()) TDataStd_Name::Set(c, TCollection_ExtendedString(std::string(l.name).c_str(), Standard_True));
    }
    st->UpdateAssemblies();
    Interface_Static::SetCVal("write.step.unit", "MM");
    Interface_Static::SetCVal("write.step.schema", "AP214IS");
    STEPCAFControl_Writer w;
    w.SetColorMode(Standard_True);
    w.SetNameMode(Standard_True);
    if (!w.Transfer(doc, STEPControl_AsIs)) fail("STEP transfer failed");
    std::ostringstream os;
    if (w.WriteStream(os) != IFSelect_RetDone) fail("cannot write STEP stream");
    app->Close(doc);
    return rust::String(os.str());
  });
}

std::unique_ptr<Shape> read_step_asm(rust::Str data, rust::Vec<AsmNode> &nodes, rust::Vec<AsmLink> &links,
                                     rust::Vec<double> &mats, rust::Vec<uint32_t> &roots) {
  return withStepLock([&] {
    Handle(XCAFApp_Application) app = XCAFApp_Application::GetApplication();
    Handle(TDocStd_Document) doc;
    app->NewDocument("MDTV-XCAF", doc);
    STEPCAFControl_Reader rd;
    rd.SetColorMode(Standard_True);
    rd.SetNameMode(Standard_True);
    rd.SetLayerMode(Standard_False);
    std::istringstream is(std::string(data.data(), data.size()));
    if (rd.ReadStream("memory.step", is) != IFSelect_RetDone) fail("STEP read failed (not a valid STEP file?)");
    if (!rd.Transfer(doc)) fail("STEP transfer failed");
    AsmRead r;
    r.st = XCAFDoc_DocumentTool::ShapeTool(doc->Main());
    r.ct = XCAFDoc_DocumentTool::ColorTool(doc->Main());
    r.nodes = &nodes;
    r.links = &links;
    r.mats = &mats;
    r.bb.MakeCompound(r.comp);
    TDF_LabelSequence free;
    r.st->GetFreeShapes(free);
    for (int i = 1; i <= free.Length(); ++i) roots.push_back(r.visit(free.Value(i), 0));
    app->Close(doc);
    if (r.nshapes == 0) fail("STEP file contains no shapes");
    return wrap(r.comp);
  });
}

std::unique_ptr<Shape> read_step_file(rust::Str path) {
  return withStepLock([&] {
    STEPControl_Reader rd;
    std::string p(path);
    return finishStepRead(rd, rd.ReadFile(p.c_str()));
  });
}

std::unique_ptr<Shape> read_step_str(rust::Str data) {
  return withStepLock([&] {
    STEPControl_Reader rd;
    std::istringstream is(std::string(data.data(), data.size()));
    return finishStepRead(rd, rd.ReadStream("memory.step", is));
  });
}

namespace {
void transferStep(STEPControl_Writer &w, const Shape &s) {
  requireShape(s, "write_step");
  Interface_Static::SetCVal("write.step.unit", "MM");
  Interface_Static::SetCVal("write.step.schema", "AP214IS");
  if (w.Transfer(s.s, STEPControl_AsIs) != IFSelect_RetDone) fail("STEP transfer failed");
}
} // namespace

void write_step_file(const Shape &s, rust::Str path) {
  return withStepLock([&] {
    STEPControl_Writer w;
    transferStep(w, s);
    std::string p(path);
    if (w.Write(p.c_str()) != IFSelect_RetDone) fail("cannot write STEP file " + p);
  });
}

rust::String write_step_str(const Shape &s) {
  return withStepLock([&] {
    STEPControl_Writer w;
    transferStep(w, s);
    std::ostringstream os;
    if (w.WriteStream(os) != IFSelect_RetDone) fail("cannot write STEP stream");
    return rust::String(os.str());
  });
}

void write_stl_file(const Shape &s, rust::Str path, double lin_defl, bool ascii) {
  requireShape(s, "write_stl");
  TopoDS_Shape m = privateCopy(s.s);
  BRepMesh_IncrementalMesh mesher(m, lin_defl, Standard_False, 0.5, Standard_False);
  if (!mesher.IsDone()) fail("meshing failed");
  StlAPI_Writer w;
  w.ASCIIMode() = ascii;
  std::string p(path);
  if (!w.Write(m, p.c_str())) fail("cannot write STL file " + p);
}

} // namespace cw
