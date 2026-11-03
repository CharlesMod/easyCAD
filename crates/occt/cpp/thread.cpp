// Modelled (helical) screw threads. Kept apart from shim.cpp: only the thread solid
// lives here.
//
// A threaded rod is a core cylinder plus a helical ridge. The ridge has a trapezoid
// cross-section in the (r, z) half-plane: `w_root` wide at `r_root`, `w_crest` wide
// at `r_crest`. Its four long faces are ruled surfaces between helices through the
// trapezoid corners (lower flank, crest, upper flank, inner band). The two short
// faces are planar caps at the start and end angle, and the faces are sewn into a
// solid. The ridge runs one pitch past each end; fusing it with the core and keeping
// the part between z = 0 and z = length gives flat, cleanly cut ends.

#include "shim.h"
#include "occt/src/ffi.rs.h"

#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <stdexcept>
#include <string>

#include <BRepAlgoAPI_Common.hxx>
#include <BRepAlgoAPI_Fuse.hxx>
#include <BRepBuilderAPI_MakeEdge.hxx>
#include <BRepBuilderAPI_MakeFace.hxx>
#include <BRepBuilderAPI_MakePolygon.hxx>
#include <BRepBuilderAPI_MakeSolid.hxx>
#include <BRepBuilderAPI_Sewing.hxx>
#include <BRepCheck_Analyzer.hxx>
#include <BRepFill.hxx>
#include <BRepLib.hxx>
#include <BRepPrimAPI_MakeCylinder.hxx>
#include <BRep_Tool.hxx>
#include <GCE2d_MakeLine.hxx>
#include <Geom2d_Line.hxx>
#include <Geom_CylindricalSurface.hxx>
#include <ShapeFix_Solid.hxx>
#include <TopExp_Explorer.hxx>
#include <TopoDS.hxx>
#include <TopoDS_Edge.hxx>
#include <TopoDS_Shell.hxx>
#include <gp_Ax2.hxx>
#include <gp_Ax3.hxx>
#include <gp_Pnt2d.hxx>

namespace cw {

namespace {

[[noreturn]] void threadFail(const std::string &msg) { throw std::runtime_error("thread: " + msg); }

// Helix of radius `r` about +Z through angle 0 at height `z0`, rising `pitch` per
// turn, over `turns` turns. Built as a line on the cylinder (exact pcurve) with an
// approximated 3D curve.
TopoDS_Edge helix(double r, double z0, double pitch, double turns) {
  Handle(Geom_CylindricalSurface) cyl = new Geom_CylindricalSurface(gp_Ax3(gp::XOY()), r);
  gp_Dir2d d(2.0 * M_PI, pitch);
  Handle(Geom2d_Line) l = new Geom2d_Line(gp_Pnt2d(0.0, z0), d);
  double len = turns * std::hypot(2.0 * M_PI, pitch);
  BRepBuilderAPI_MakeEdge me(l, cyl, 0.0, len);
  if (!me.IsDone()) threadFail("cannot build a helix");
  TopoDS_Edge e = me.Edge();
  int segs = std::max(30, static_cast<int>(std::ceil(turns * 8.0)));
  if (!BRepLib::BuildCurves3d(e, 1.0e-6, GeomAbs_C2, 8, segs)) threadFail("cannot approximate a helix");
  return e;
}

TopoDS_Face ruled(const TopoDS_Edge &a, const TopoDS_Edge &b) {
  TopoDS_Face f = BRepFill::Face(a, b);
  if (f.IsNull()) threadFail("cannot build a flank");
  return f;
}

gp_Pnt endPoint(const TopoDS_Edge &e, bool last) {
  double f, l;
  Handle(Geom_Curve) c = BRep_Tool::Curve(e, f, l);
  if (c.IsNull()) threadFail("helix without a 3D curve");
  return c->Value(last ? l : f);
}

TopoDS_Face cap(const TopoDS_Edge (&h)[4], bool last) {
  BRepBuilderAPI_MakePolygon poly;
  for (const auto &e : h) poly.Add(endPoint(e, last));
  poly.Close();
  BRepBuilderAPI_MakeFace mf(poly.Wire(), Standard_True);
  if (!mf.IsDone()) threadFail("cannot cap the ridge");
  return mf.Face();
}

} // namespace

std::unique_ptr<Shape> thread_solid(double r_root, double r_crest, double pitch, double length, double w_root,
                                    double w_crest) {
  if (!(pitch > 0.0) || !(length > 0.0) || !(r_root > 0.0) || !(r_crest > r_root)) threadFail("bad dimensions");
  if (!(w_crest >= 0.0) || !(w_root > w_crest)) threadFail("bad profile widths");
  auto T0 = std::chrono::steady_clock::now();
  auto lap = [&](const char *w) {
    if (std::getenv("CW_PROF")) fprintf(stderr, "  thread %s: %.1f ms\n", w, std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - T0).count());
  };
  // the ridge sinks a little into the core so the fuse never meets tangent faces
  double depth = r_crest - r_root;
  double sink = 0.1 * depth;
  double slope = 0.5 * (w_root - w_crest) / depth; // axial per radial along a flank
  double r_in = r_root - sink;
  double w_in = w_root + 2.0 * sink * slope;
  if (w_in >= 0.98 * pitch) threadFail("the profile is wider than the pitch");
  // corners: inner low, crest low, crest high, inner high (z relative to the helix)
  double z0 = -pitch; // one pitch before the start
  double turns = length / pitch + 2.0;
  TopoDS_Edge h[4] = {
      helix(r_in, z0 - 0.5 * w_in, pitch, turns),
      helix(r_crest, z0 - 0.5 * w_crest, pitch, turns),
      helix(r_crest, z0 + 0.5 * w_crest, pitch, turns),
      helix(r_in, z0 + 0.5 * w_in, pitch, turns),
  };
  BRepBuilderAPI_Sewing sew(1.0e-5);
  for (int i = 0; i < 4; ++i) {
    if (i == 1 && w_crest <= 1.0e-9) continue; // sharp crest: no crest band
    sew.Add(ruled(h[i], h[(i + 1) % 4]));
  }
  sew.Add(cap(h, false));
  sew.Add(cap(h, true));
  sew.Perform();
  TopoDS_Shape sewn = sew.SewedShape();
  TopoDS_Shell shell;
  int nShells = 0;
  for (TopExp_Explorer ex(sewn, TopAbs_SHELL); ex.More(); ex.Next(), ++nShells) shell = TopoDS::Shell(ex.Current());
  if (nShells != 1) threadFail("the ridge faces do not sew into one shell");
  BRepBuilderAPI_MakeSolid ms(shell);
  if (!ms.IsDone()) threadFail("the ridge is not closed");
  ShapeFix_Solid fix(ms.Solid());
  fix.Perform();
  TopoDS_Shape ridge = fix.Solid();
  lap("ridge");

  TopoDS_Shape core = BRepPrimAPI_MakeCylinder(gp_Ax2(gp_Pnt(0, 0, -2.0 * pitch), gp::DZ()), r_root, length + 4.0 * pitch).Shape();
  BRepAlgoAPI_Fuse fuse(core, ridge);
  if (!fuse.IsDone() || fuse.HasErrors()) threadFail("cannot join the ridge to the core");
  lap("fuse");
  TopoDS_Shape keep = BRepPrimAPI_MakeCylinder(gp_Ax2(gp_Pnt(0, 0, 0), gp::DZ()), r_crest + 1.0, length).Shape();
  BRepAlgoAPI_Common common(fuse.Shape(), keep);
  if (!common.IsDone() || common.HasErrors()) threadFail("cannot trim the thread ends");
  TopoDS_Shape out = common.Shape();
  lap("common");
  TopoDS_Shape solid;
  int n = 0;
  for (TopExp_Explorer ex(out, TopAbs_SOLID); ex.More(); ex.Next(), ++n) solid = ex.Current();
  if (n != 1) threadFail("the threaded rod is not one solid");
  if (!BRepCheck_Analyzer(solid).IsValid()) threadFail("the threaded rod is not a valid solid");
  lap("check");
  return std::make_unique<Shape>(solid);
}

} // namespace cw
