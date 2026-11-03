#pragma once
// Narrow C++ shim over OpenCascade for the Casework cxx bridge.

#include <exception>
#include <memory>
#include <string>

#include <Standard_Failure.hxx>
#include <TopoDS_Shape.hxx>

#include "rust/cxx.h"

// Convert every C++/OCCT exception into a Rust `cxx::Exception` instead of aborting.
// Standard_Failure does not derive from std::exception, so cxx's default would not
// catch it.
namespace rust {
namespace behavior {
template <typename Try, typename Fail>
static void trycatch(Try &&func, Fail &&fail) noexcept try {
  func();
} catch (const Standard_Failure &e) {
  std::string msg = e.DynamicType()->Name();
  const char *m = e.GetMessageString();
  if (m != nullptr && *m != '\0') {
    msg += ": ";
    msg += m;
  }
  fail(msg.c_str());
} catch (const std::exception &e) {
  fail(e.what());
} catch (...) {
  fail("unknown C++ exception");
}
} // namespace behavior
} // namespace rust

namespace cw {

class Shape {
public:
  Shape() = default;
  explicit Shape(TopoDS_Shape s) : s(std::move(s)) {}
  TopoDS_Shape s;
};

struct V3;
struct Frame;
struct EdgeSpec;
struct FaceInfo;
struct EdgeInfo;
struct MeshData;
struct BBox;
struct MassProps;

std::unique_ptr<Shape> shape_clone(const Shape &s);
bool shape_is_null(const Shape &s);
uint32_t shape_type(const Shape &s);
bool shape_is_valid(const Shape &s);
uint32_t count_sub(const Shape &s, uint32_t kind);
std::unique_ptr<Shape> sub_shape(const Shape &s, uint32_t kind, uint32_t index);
std::unique_ptr<Shape> compound_new();
void compound_add(Shape &c, const Shape &s);

std::unique_ptr<Shape> make_box(V3 origin, double dx, double dy, double dz);
std::unique_ptr<Shape> make_cylinder(V3 base, V3 dir, double r, double h);
std::unique_ptr<Shape> make_cone(V3 base, V3 dir, double r1, double r2, double h);

std::unique_ptr<Shape> face_from_edges(const Frame &frame, rust::Slice<const EdgeSpec> edges);
std::unique_ptr<Shape> extrude(const Shape &s, V3 v);
std::unique_ptr<Shape> extrude_draft(const Shape &s, V3 v, double angle_rad, const Frame &neutral);
std::unique_ptr<Shape> revolve(const Shape &s, V3 origin, V3 dir, double angle_rad);

std::unique_ptr<Shape> boolean(const Shape &a, const Shape &b, uint32_t op);
std::unique_ptr<Shape> fuse_all(const Shape &c);
std::unique_ptr<Shape> unify(const Shape &s);
std::unique_ptr<Shape> heal(const Shape &s);
rust::Vec<uint32_t> tangent_chains(const Shape &s);
int32_t edge_convexity(const Shape &s, uint32_t edge);
int32_t match_edge(const Shape &s, V3 p, V3 tangent, double tol);

std::unique_ptr<Shape> fillet(const Shape &s, rust::Slice<const uint32_t> edges, rust::Slice<const double> radii);
std::unique_ptr<Shape> chamfer(const Shape &s, rust::Slice<const uint32_t> edges, rust::Slice<const double> dists);
std::unique_ptr<Shape> shell(const Shape &s, rust::Slice<const uint32_t> faces, double offset);
std::unique_ptr<Shape> draft(const Shape &s, rust::Slice<const uint32_t> faces, V3 pull,
                             double angle_rad, const Frame &neutral);

std::unique_ptr<Shape> translate(const Shape &s, V3 v);
std::unique_ptr<Shape> rotate(const Shape &s, V3 origin, V3 dir, double angle_rad);
std::unique_ptr<Shape> mirror(const Shape &s, V3 origin, V3 normal);
std::unique_ptr<Shape> half_space_side(const Shape &s, V3 origin, V3 normal, bool above);

rust::Vec<FaceInfo> faces(const Shape &s);
rust::Vec<EdgeInfo> edges(const Shape &s);
rust::Vec<uint32_t> face_edges(const Shape &s, uint32_t face);
MeshData tessellate(const Shape &s, double lin_defl, double ang_defl_rad);

MassProps mass_props(const Shape &s);
BBox bounding_box(const Shape &s);

std::unique_ptr<Shape> read_step_file(rust::Str path);
std::unique_ptr<Shape> read_step_str(rust::Str data);
void write_step_file(const Shape &s, rust::Str path);
rust::String write_step_str(const Shape &s);
void write_stl_file(const Shape &s, rust::Str path, double lin_defl, bool ascii);

} // namespace cw
