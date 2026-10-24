// step_tess: tessellate a STEP file into LiNC's ASCII .lincmesh format.
//
// Usage: step_tess <input.step> <output.lincmesh>
//
// Output format (version 1):
//   LINC3D-MESH 1
//   MM
//   V <x> <y> <z>
//   F <a> <b> <c>
// Positions are millimetres, indices are 0-based and refer to the V lines
// emitted so far. Every triangulation of every face of the whole compound is
// emitted; vertices are not deduplicated because the machine simulator only
// needs triangles.
#include <BRepMesh_IncrementalMesh.hxx>
#include <BRep_Tool.hxx>
#include <IFSelect_ReturnStatus.hxx>
#include <Poly_Triangulation.hxx>
#include <STEPControl_Reader.hxx>
#include <TopAbs_Orientation.hxx>
#include <TopExp_Explorer.hxx>
#include <TopLoc_Location.hxx>
#include <TopoDS.hxx>
#include <TopoDS_Face.hxx>
#include <TopoDS_Shape.hxx>
#include <gp_Pnt.hxx>
#include <gp_Trsf.hxx>

#include <cstdio>
#include <cstdlib>
#include <cmath>
#include <vector>

namespace {

struct Vertex {
  double x, y, z;
};

struct Triangle {
  int a, b, c;
};

bool tessellate(const TopoDS_Shape& shape, std::vector<Vertex>& verts,
                std::vector<Triangle>& tris) {
  for (TopExp_Explorer ex(shape, TopAbs_FACE); ex.More(); ex.Next()) {
    const TopoDS_Face face = TopoDS::Face(ex.Current());
    TopLoc_Location loc;
    Handle(Poly_Triangulation) tri = BRep_Tool::Triangulation(face, loc);
    if (tri.IsNull()) continue;
    const gp_Trsf trsf = loc.Transformation();
    const bool reversed = (face.Orientation() == TopAbs_REVERSED);
    for (Standard_Integer i = 1; i <= tri->NbTriangles(); ++i) {
      Standard_Integer n1 = 0, n2 = 0, n3 = 0;
      tri->Triangle(i).Get(n1, n2, n3);
      if (n1 < 1 || n2 < 1 || n3 < 1) continue;
      if (n2 > tri->NbNodes() || n3 > tri->NbNodes()) continue;
      if (reversed) std::swap(n2, n3);
      const int idx[3] = {n1, n2, n3};
      Triangle t{};
      int* out = &t.a;
      for (int k = 0; k < 3; ++k) {
        gp_Pnt p = tri->Node(idx[k]);
        p.Transform(trsf);
        if (!std::isfinite(p.X()) || !std::isfinite(p.Y()) ||
            !std::isfinite(p.Z()))
          return false;
        out[k] = static_cast<int>(verts.size());
        verts.push_back(Vertex{p.X(), p.Y(), p.Z()});
      }
      tris.push_back(t);
    }
  }
  return true;
}

}  // namespace

int main(int argc, char** argv) {
  if (argc != 3) {
    std::fprintf(stderr, "usage: %s <input.step> <output.lincmesh>\n", argv[0]);
    return 2;
  }
  const char* in_path = argv[1];
  const char* out_path = argv[2];

  STEPControl_Reader reader;
  const IFSelect_ReturnStatus read_status = reader.ReadFile(in_path);
  if (read_status != IFSelect_RetDone) {
    std::fprintf(stderr, "step_tess: cannot read '%s' (status %d)\n", in_path,
                 static_cast<int>(read_status));
    return 3;
  }
  // The transfer process converts the file's length unit into this one; 1.0 is
  // one millimetre, so every emitted position is in millimetres.
  reader.SetSystemLengthUnit(1.0);
  const Standard_Integer n_roots = reader.TransferRoots();
  if (n_roots <= 0) {
    std::fprintf(stderr, "step_tess: no transferable roots in '%s'\n", in_path);
    return 4;
  }
  TopoDS_Shape shape = reader.OneShape();
  if (shape.IsNull()) {
    std::fprintf(stderr, "step_tess: empty shape in '%s'\n", in_path);
    return 5;
  }

  const double linear_deflection = 0.05;  // mm
  const double angular_deflection = 0.3;  // rad
  BRepMesh_IncrementalMesh mesher(shape, linear_deflection, Standard_False,
                                  angular_deflection, Standard_True);
  if (!mesher.IsDone()) {
    std::fprintf(stderr, "step_tess: tessellation failed for '%s'\n", in_path);
    return 6;
  }

  std::vector<Vertex> verts;
  std::vector<Triangle> tris;
  if (!tessellate(shape, verts, tris)) {
    std::fprintf(stderr, "step_tess: non-finite vertex in '%s'\n", in_path);
    return 7;
  }
  if (tris.empty()) {
    std::fprintf(stderr, "step_tess: no triangles in '%s'\n", in_path);
    return 8;
  }

  std::FILE* out = std::fopen(out_path, "wb");
  if (out == nullptr) {
    std::fprintf(stderr, "step_tess: cannot open '%s' for writing\n", out_path);
    return 9;
  }
  std::fprintf(out, "LINC3D-MESH 1\nMM\n");
  char line[192];
  for (const Vertex& v : verts) {
    std::snprintf(line, sizeof line, "V %.6f %.6f %.6f\n", v.x, v.y, v.z);
    std::fputs(line, out);
  }
  for (const Triangle& t : tris) {
    std::snprintf(line, sizeof line, "F %d %d %d\n", t.a, t.b, t.c);
    std::fputs(line, out);
  }
  if (std::fclose(out) != 0) {
    std::fprintf(stderr, "step_tess: write failed for '%s'\n", out_path);
    return 10;
  }
  std::fprintf(stderr, "step_tess: %zu vertices, %zu triangles\n", verts.size(),
               tris.size());
  return 0;
}
