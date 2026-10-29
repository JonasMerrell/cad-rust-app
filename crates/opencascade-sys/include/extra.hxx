// Extra OpenCascade operations for cad-rust-app. Every function catches OCCT
// exceptions and returns a null shape instead (cxx can't translate
// Standard_Failure, and an uncaught one aborts the process). The message of
// the last failure is available from ex_last_error().
#pragma once
#include "rust/cxx.h"
#include <memory>
#include <string>

#include <BRepAlgoAPI_Common.hxx>
#include <BRepMesh_IncrementalMesh.hxx>
#include <IMeshTools_Parameters.hxx>
#include <BRepTools.hxx>
#include <BRepOffset_MakeOffset.hxx>
#include <TopTools_IndexedDataMapOfShapeListOfShape.hxx>
#include <TopExp.hxx>
#include <BRepBuilderAPI_Sewing.hxx>
#include <BRepBuilderAPI_MakeSolid.hxx>
#include <BRepOffsetAPI_MakeFilling.hxx>
#include <BRepOffsetAPI_MakeOffsetShape.hxx>
#include <GeomAbs_Shape.hxx>
#include <BRepAlgoAPI_Cut.hxx>
#include <BRepAlgoAPI_Defeaturing.hxx>
#include <BRepAlgoAPI_Fuse.hxx>
#include <BRepBuilderAPI_MakeEdge.hxx>
#include <BRepBuilderAPI_MakeFace.hxx>
#include <BRepBuilderAPI_MakeWire.hxx>
#include <BRepBuilderAPI_Transform.hxx>
#include <BRepCheck_Analyzer.hxx>
#include <BRepFilletAPI_MakeChamfer.hxx>
#include <BRepFilletAPI_MakeFillet.hxx>
#include <BRepLib.hxx>
#include <BRepGProp.hxx>
#include <GProp_GProps.hxx>
#include <BRepOffsetAPI_DraftAngle.hxx>
#include <BRepOffsetAPI_MakePipeShell.hxx>
#include <BRepOffsetAPI_MakeThickSolid.hxx>
#include <BRepOffsetAPI_ThruSections.hxx>
#include <BRepPrimAPI_MakePrism.hxx>
#include <BRepPrimAPI_MakeRevol.hxx>
#include <GC_MakeArcOfEllipse.hxx>
#include <GCE2d_MakeSegment.hxx>
#include <Geom2d_Line.hxx>
#include <GeomAPI_Interpolate.hxx>
#include <Geom_BSplineCurve.hxx>
#include <Geom_CylindricalSurface.hxx>
#include <Geom_Ellipse.hxx>
#include <HLRAlgo_Projector.hxx>
#include <HLRBRep_Algo.hxx>
#include <HLRBRep_HLRToShape.hxx>
#include <ShapeFix_Shape.hxx>
#include <ShapeFix_Wireframe.hxx>
#include <Bnd_Box.hxx>
#include <BRepBndLib.hxx>
#include <ShapeFix_FixSmallFace.hxx>
#include <ShapeBuild_ReShape.hxx>
#include <ShapeUpgrade_UnifySameDomain.hxx>
#include <Standard_Failure.hxx>
#include <TColgp_HArray1OfPnt.hxx>
#include <TopExp_Explorer.hxx>
#include <TopTools_ListOfShape.hxx>
#include <TopoDS.hxx>
#include <TopoDS_Compound.hxx>
#include <TopTools_IndexedMapOfShape.hxx>
#include <Geom_Plane.hxx>
#include <Geom_Line.hxx>
#include <Poly_Triangulation.hxx>
#include <Poly_Triangle.hxx>
#include <TopoDS_Wire.hxx>
#include <BRep_Builder.hxx>
#include <BRepBuilderAPI_MakeVertex.hxx>
#include <BRepCheck_Analyzer.hxx>
#include <BRep_Tool.hxx>
#include <TopoDS_Shell.hxx>
#include <TopoDS_Solid.hxx>
#include <gp_Pln.hxx>
#include <map>
#include <unordered_map>
#include <vector>
#include <gp_Ax2.hxx>
#include <gp_Ax3.hxx>
#include <gp_Elips.hxx>
#include <gp_Pln.hxx>
#include <gp_Trsf.hxx>

inline std::string &ex_err_slot() {
  static thread_local std::string s;
  return s;
}
inline rust::String ex_last_error() { return rust::String(ex_err_slot()); }

inline std::unique_ptr<TopoDS_Shape> ex_wrap(const TopoDS_Shape &s) { return std::unique_ptr<TopoDS_Shape>(new TopoDS_Shape(s)); }
inline std::unique_ptr<TopoDS_Shape> ex_null(const char *why) {
  ex_err_slot() = why;
  return std::unique_ptr<TopoDS_Shape>(new TopoDS_Shape());
}

#define EX_TRY try {
#define EX_CATCH(what)                                                                                                 \
  }                                                                                                                    \
  catch (const Standard_Failure &e) {                                                                                  \
    std::string m = std::string(what) + ": " + e.GetMessageString();                                                  \
    ex_err_slot() = m;                                                                                                 \
    return std::unique_ptr<TopoDS_Shape>(new TopoDS_Shape());                                                         \
  }                                                                                                                    \
  catch (...) { return ex_null(what " failed"); }

inline std::unique_ptr<TopoDS_Shape> ex_fixed(const TopoDS_Shape &s) {
  Handle(ShapeFix_Shape) fix = new ShapeFix_Shape(s);
  fix->Perform();
  return ex_wrap(fix->Shape());
}

/// Merge coplanar faces and collinear edges left over by modelling operations.
inline TopoDS_Shape ex_unify_raw(const TopoDS_Shape &s) {
  try {
    // faceted shapes (imported meshes) are already merged, and unifying thousands of faces is slow
    TopTools_IndexedMapOfShape fm;
    TopExp::MapShapes(s, TopAbs_FACE, fm);
    if (fm.Extent() >= 1500)
      return s;
    ShapeUpgrade_UnifySameDomain u(s, Standard_True, Standard_True, Standard_False);
    u.Build();
    TopoDS_Shape r = u.Shape();
    return r.IsNull() ? s : r;
  } catch (...) {
    return s;
  }
}

inline std::unique_ptr<TopoDS_Shape> ex_unify(const TopoDS_Shape &s) { return ex_wrap(ex_unify_raw(s)); }

// A solid (or shell, if it doesn't close) from a triangle mesh: `pts` holds unique
// vertices (xyz), `tris` vertex indices. Neighbouring triangles share edges, so no
// sewing is needed; with `merge`, coplanar triangles are merged into larger faces.
// `faces` is a flat list, per face: [nloops, (n, v0..vn-1)*nloops, ntris, (a, b, c)*ntris];
// the first loop is the outer boundary (counter-clockwise about the normal), the rest holes,
// and the triangles (vertex indices) are the face's display triangulation.
inline std::unique_ptr<TopoDS_Shape> ex_mesh_solid(rust::Slice<const double> pts, rust::Slice<const uint32_t> faces) {
  EX_TRY
  // built directly with BRep_Builder: the API makers check too much to be fast on big meshes
  const double tol = 1e-4; // covers float rounding in STL files
  BRep_Builder bb;
  size_t nv = pts.size() / 3;
  std::vector<TopoDS_Vertex> verts(nv);
  std::vector<gp_Pnt> P(nv);
  for (size_t i = 0; i < nv; i++) {
    P[i] = gp_Pnt(pts[3 * i], pts[3 * i + 1], pts[3 * i + 2]);
    bb.MakeVertex(verts[i], P[i], tol);
  }
  // edge index by vertex pair; each entry also counts how many triangles use it
  std::unordered_map<uint64_t, std::pair<TopoDS_Edge, int>> edges;
  edges.reserve(faces.size());
  auto edge = [&](uint32_t a, uint32_t b) -> TopoDS_Edge {
    auto key = a < b ? std::make_pair(a, b) : std::make_pair(b, a);
    uint64_t k = (uint64_t(key.first) << 32) | key.second;
    auto it = edges.find(k);
    if (it != edges.end()) {
      it->second.second++;
    } else {
      const gp_Pnt &p = P[key.first], &q = P[key.second];
      double len = p.Distance(q);
      Handle(Geom_Line) ln = new Geom_Line(p, gp_Dir(gp_Vec(p, q)));
      TopoDS_Edge e;
      bb.MakeEdge(e, ln, tol);
      TopoDS_Vertex v1 = TopoDS::Vertex(verts[key.first].Oriented(TopAbs_FORWARD));
      TopoDS_Vertex v2 = TopoDS::Vertex(verts[key.second].Oriented(TopAbs_REVERSED));
      bb.Add(e, v1);
      bb.Add(e, v2);
      bb.UpdateVertex(v1, 0.0, e, tol);
      bb.UpdateVertex(v2, len, e, tol);
      bb.Range(e, 0.0, len);
      it = edges.emplace(k, std::make_pair(e, 1)).first;
    }
    return a == key.first ? it->second.first : TopoDS::Edge(it->second.first.Reversed());
  };
  TopoDS_Shell shell;
  bb.MakeShell(shell);
  size_t nfaces = 0;
  double vol6 = 0; // six times the signed volume
  size_t at = 0;
  auto next = [&]() -> uint32_t { return at < faces.size() ? faces[at++] : 0; };
  while (at < faces.size()) {
    uint32_t nloops = next();
    std::vector<std::vector<uint32_t>> loops(nloops);
    for (auto &l : loops) {
      uint32_t n = next();
      for (uint32_t k = 0; k < n; k++)
        l.push_back(next());
    }
    uint32_t nt = next();
    std::vector<uint32_t> tri(3 * nt);
    for (auto &x : tri)
      x = next();
    if (loops.empty() || loops[0].size() < 3 || nt == 0)
      continue;
    bool bad = false;
    for (auto x : tri)
      if (x >= nv)
        bad = true;
    for (auto &l : loops)
      for (auto x : l)
        if (x >= nv)
          bad = true;
    if (bad)
      continue;
    // plane from the triangles (area-weighted normal)
    gp_XYZ nsum(0, 0, 0);
    for (uint32_t t = 0; t < nt; t++) {
      const gp_Pnt &a = P[tri[3 * t]], &b = P[tri[3 * t + 1]], &c = P[tri[3 * t + 2]];
      gp_XYZ n = (b.XYZ() - a.XYZ()).Crossed(c.XYZ() - a.XYZ());
      nsum += n;
      vol6 += a.XYZ().Dot(b.XYZ().Crossed(c.XYZ()));
    }
    if (nsum.Modulus() < 1e-14)
      continue;
    Handle(Geom_Plane) pl = new Geom_Plane(P[loops[0][0]], gp_Dir(nsum));
    TopoDS_Face f;
    bb.MakeFace(f, pl, tol);
    for (auto &l : loops) {
      TopoDS_Wire w;
      bb.MakeWire(w);
      for (size_t k = 0; k < l.size(); k++)
        bb.Add(w, edge(l[k], l[(k + 1) % l.size()]));
      w.Closed(Standard_True);
      bb.Add(f, w);
    }
    // the face's own triangles, so display needs no meshing
    std::map<uint32_t, int> local;
    for (auto x : tri)
      local.emplace(x, 0);
    int ni = 0;
    for (auto &kv : local)
      kv.second = ++ni;
    Handle(Poly_Triangulation) pt = new Poly_Triangulation(ni, nt, Standard_False);
    for (auto &kv : local)
      pt->SetNode(kv.second, P[kv.first]);
    for (uint32_t t = 0; t < nt; t++)
      pt->SetTriangle(t + 1, Poly_Triangle(local[tri[3 * t]], local[tri[3 * t + 1]], local[tri[3 * t + 2]]));
    pt->Deflection(0.0);
    bb.UpdateFace(f, pt);
    bb.Add(shell, f);
    nfaces++;
  }
  if (nfaces == 0)
    return ex_null("the mesh has no usable triangles");
  TopoDS_Shape result = shell;
  BRep_Builder sb;
  TopoDS_Solid solid;
  sb.MakeSolid(solid);
  sb.Add(solid, shell);
  bool closed = true;
  for (auto &u : edges)
    if (u.second.second != 2)
      closed = false;
  shell.Closed(closed);
  if (closed) {
    if (vol6 < 0)
      solid.Reverse();
    result = solid;
  }
  return ex_wrap(result);  EX_CATCH("mesh import")
}

// Repair a part: remove edges and faces smaller than `tol`, sew gaps up to `tol`,
// fix tolerances and orientation, merge faces that lie on the same surface.
// Points on the faces and edges BRepCheck finds invalid, as xyz triples.
inline rust::Vec<double> ex_invalid_spots(const TopoDS_Shape &s) {
  rust::Vec<double> out;
  try {
    BRepCheck_Analyzer chk(s);
    if (chk.IsValid())
      return out;
    for (TopAbs_ShapeEnum t : {TopAbs_FACE, TopAbs_EDGE}) {
      for (TopExp_Explorer ex(s, t); ex.More(); ex.Next()) {
        if (chk.IsValid(ex.Current()))
          continue;
        Bnd_Box b;
        BRepBndLib::Add(ex.Current(), b);
        if (b.IsVoid())
          continue;
        double x0, y0, z0, x1, y1, z1;
        b.Get(x0, y0, z0, x1, y1, z1);
        out.push_back((x0 + x1) / 2);
        out.push_back((y0 + y1) / 2);
        out.push_back((z0 + z1) / 2);
      }
      if (!out.empty())
        break;
    }
  } catch (...) {
  }
  return out;
}

inline double ex_solid_volume(const TopoDS_Shape &s) {
  double v = 0;
  for (TopExp_Explorer ex(s, TopAbs_SOLID); ex.More(); ex.Next()) {
    GProp_GProps props;
    BRepGProp::VolumeProperties(ex.Current(), props);
    v += props.Mass();
  }
  return v;
}

inline TopoDS_Shape ex_as_solid(const TopoDS_Shape &r) {
  TopExp_Explorer has(r, TopAbs_SOLID);
  if (has.More())
    return r;
  BRepBuilderAPI_MakeSolid ms;
  bool any = false;
  for (TopExp_Explorer ex(r, TopAbs_SHELL); ex.More(); ex.Next()) {
    ms.Add(TopoDS::Shell(ex.Current()));
    any = true;
  }
  if (!any || !ms.IsDone())
    return r;
  TopoDS_Shape so = ms.Shape();
  if (ex_solid_volume(so) < 0)
    so.Reverse();
  return so;
}

// Repair a part: close gaps up to `tol`, drop edges and faces smaller than it, fix
// tolerances, merge faces on the same surface. Each step is kept only if the part
// stays a valid solid of (almost) the same volume, so healing never breaks a part.
inline std::unique_ptr<TopoDS_Shape> ex_heal(const TopoDS_Shape &s, double tol) {
  EX_TRY
  TopoDS_Shape r = s;
  double v0 = ex_solid_volume(s);
  bool was_solid = v0 > 0;
  auto accept = [&](TopoDS_Shape c) {
    if (c.IsNull())
      return;
    if (was_solid) {
      c = ex_as_solid(c);
      double v = ex_solid_volume(c);
      if (v <= 0 || std::abs(v - v0) > 0.01 * v0)
        return;
      BRepCheck_Analyzer chk(c);
      BRepCheck_Analyzer before(r);
      if (!chk.IsValid() && before.IsValid())
        return;
    }
    r = c;
  };
  {
    BRepBuilderAPI_Sewing sew(tol);
    sew.Add(r);
    sew.Perform();
    accept(sew.SewedShape());
  }
  {
    Handle(ShapeFix_Wireframe) wf = new ShapeFix_Wireframe(r);
    wf->SetPrecision(tol);
    wf->SetMaxTolerance(tol * 10);
    wf->ModeDropSmallEdges() = Standard_True;
    wf->FixSmallEdges();
    wf->FixWireGaps();
    accept(wf->Shape());
  }
  {
    ShapeFix_FixSmallFace sf;
    sf.Init(r);
    sf.SetPrecision(tol);
    sf.SetMaxTolerance(tol * 10);
    sf.Perform();
    accept(sf.FixShape());
  }
  {
    Handle(ShapeFix_Shape) fix = new ShapeFix_Shape(r);
    fix->SetPrecision(tol);
    fix->SetMaxTolerance(tol * 10);
    fix->Perform();
    accept(fix->Shape());
  }
  accept(ex_unify_raw(r));
  return ex_wrap(r);
  EX_CATCH("heal")
}

// Edges worth drawing: every edge between two faces, except, on faceted shapes
// (imported meshes), the ones between faces meeting at less than `angle` (radians).
inline std::unique_ptr<TopoDS_Shape> ex_feature_edges(const TopoDS_Shape &s, double angle, int faceted_from) {
  EX_TRY
  TopTools_IndexedDataMapOfShapeListOfShape map;
  TopExp::MapShapesAndUniqueAncestors(s, TopAbs_EDGE, TopAbs_FACE, map);
  TopTools_IndexedMapOfShape faces;
  TopExp::MapShapes(s, TopAbs_FACE, faces);
  bool faceted = faces.Extent() >= faceted_from;
  BRep_Builder bb;
  TopoDS_Compound out;
  bb.MakeCompound(out);
  for (int i = 1; i <= map.Extent(); i++) {
    const TopTools_ListOfShape &fl = map.FindFromIndex(i);
    if (fl.Size() == 1)
      continue;
    if (faceted && fl.Size() == 2) {
      TopoDS_Face f1 = TopoDS::Face(fl.First()), f2 = TopoDS::Face(fl.Last());
      Handle(Geom_Plane) p1 = Handle(Geom_Plane)::DownCast(BRep_Tool::Surface(f1));
      Handle(Geom_Plane) p2 = Handle(Geom_Plane)::DownCast(BRep_Tool::Surface(f2));
      if (!p1.IsNull() && !p2.IsNull()) {
        gp_Dir n1 = p1->Axis().Direction(), n2 = p2->Axis().Direction();
        if (f1.Orientation() == TopAbs_REVERSED) n1.Reverse();
        if (f2.Orientation() == TopAbs_REVERSED) n2.Reverse();
        if (n1.Angle(n2) < angle)
          continue;
      }
    }
    bb.Add(out, map.FindKey(i));
  }
  return ex_wrap(out);
  EX_CATCH("edges")
}

// Triangulate with a linear and an angular limit (radians); replaces coarser meshes.
inline bool ex_mesh(const TopoDS_Shape &s, double linear, double angular) {
  try {
    // drop triangulations made earlier with other limits, or they would be kept;
    // faceted shapes (imported meshes) keep theirs, which are exact
    TopTools_IndexedMapOfShape faces;
    TopExp::MapShapes(s, TopAbs_FACE, faces);
    if (faces.Extent() < 1500) {
      BRepTools::Clean(s);
    } else {
      // mesh only the faces that have no triangulation yet (e.g. new faces from a cut)
      BRep_Builder bb;
      TopoDS_Compound todo;
      bb.MakeCompound(todo);
      int n = 0;
      for (int i = 1; i <= faces.Extent(); i++) {
        TopLoc_Location loc;
        if (BRep_Tool::Triangulation(TopoDS::Face(faces(i)), loc).IsNull()) {
          bb.Add(todo, faces(i));
          n++;
        }
      }
      if (n == 0)
        return true;
      BRepMesh_IncrementalMesh m(todo, linear, Standard_False, angular, Standard_True);
      return m.IsDone();
    }
    BRepMesh_IncrementalMesh m(s, linear, Standard_False, angular, Standard_True);
    return m.IsDone();
  } catch (...) {
    return false;
  }
}

// Remesh from scratch with more careful settings, for when the normal mesh has gaps.
inline bool ex_mesh_careful(const TopoDS_Shape &s, double linear, double angular, int level) {
  try {
    BRepTools::Clean(s);
    IMeshTools_Parameters p;
    p.Deflection = linear / (level + 1);
    p.Angle = angular;
    p.InParallel = Standard_False;
    p.Relative = Standard_False;
    p.InternalVerticesMode = Standard_True;
    p.ControlSurfaceDeflection = Standard_True;
    p.MinSize = linear * 0.01;
    p.AllowQualityDecrease = level >= 2;
    BRepMesh_IncrementalMesh m(s, p);
    return m.IsDone();
  } catch (...) {
    return false;
  }
}

inline TopoDS_Shape ex_boolean_raw(const TopoDS_Shape &a, const TopoDS_Shape &b, int op, double fuzzy) {
  TopTools_ListOfShape args, tools;
  args.Append(a);
  tools.Append(b);
  std::unique_ptr<BRepAlgoAPI_BooleanOperation> bop;
  if (op == 0)
    bop.reset(new BRepAlgoAPI_Fuse());
  else if (op == 1)
    bop.reset(new BRepAlgoAPI_Cut());
  else
    bop.reset(new BRepAlgoAPI_Common());
  bop->SetArguments(args);
  bop->SetTools(tools);
  bop->SetFuzzyValue(fuzzy);
  bop->SetRunParallel(true);
  bop->Build();
  if (!bop->IsDone() || bop->HasErrors())
    return TopoDS_Shape();
  return bop->Shape();
}

inline int ex_face_count(const TopoDS_Shape &s) {
  TopTools_IndexedMapOfShape fm;
  TopExp::MapShapes(s, TopAbs_FACE, fm);
  return fm.Extent();
}

// Boolean with a retry: when faces nearly touch (within a few microns) the exact
// operation can leave slivers or invalid faces, so if the result fails the check it
// is redone with a larger fuzzy tolerance that treats such near-misses as touching.
inline std::unique_ptr<TopoDS_Shape> ex_boolean(const TopoDS_Shape &a, const TopoDS_Shape &b, int op) {
  EX_TRY
  TopoDS_Shape r = ex_boolean_raw(a, b, op, 1e-6);
  bool check = ex_face_count(a) + ex_face_count(b) < 1500;
  if (check && (r.IsNull() || !BRepCheck_Analyzer(r).IsValid())) {
    Bnd_Box bx;
    BRepBndLib::Add(a, bx);
    BRepBndLib::Add(b, bx);
    double diag = bx.IsVoid() ? 1.0 : std::sqrt(bx.SquareExtent());
    for (double f : {1e-5 * diag, 1e-4 * diag}) {
      TopoDS_Shape r2 = ex_boolean_raw(a, b, op, std::min(f, 0.05));
      if (!r2.IsNull() && BRepCheck_Analyzer(r2).IsValid()) {
        r = r2;
        break;
      }
    }
  }
  if (r.IsNull())
    return ex_null("boolean operation failed");
  return ex_wrap(ex_unify_raw(r));
  EX_CATCH("boolean")
}

inline std::unique_ptr<TopoDS_Shape> ex_prism(const TopoDS_Shape &profile, double dx, double dy, double dz) {
  EX_TRY
  BRepPrimAPI_MakePrism mk(profile, gp_Vec(dx, dy, dz));
  mk.Build();
  if (!mk.IsDone())
    return ex_null("extrude failed");
  return ex_wrap(ex_unify_raw(mk.Shape()));
  EX_CATCH("extrude")
}

inline std::unique_ptr<TopoDS_Shape> ex_revol(const TopoDS_Shape &profile, double ox, double oy, double oz, double dx,
                                               double dy, double dz, double angle) {
  EX_TRY
  BRepPrimAPI_MakeRevol mk(profile, gp_Ax1(gp_Pnt(ox, oy, oz), gp_Dir(dx, dy, dz)), angle);
  mk.Build();
  if (!mk.IsDone())
    return ex_null("revolve failed");
  return ex_wrap(mk.Shape());
  EX_CATCH("revolve")
}

inline std::unique_ptr<TopoDS_Shape> ex_fillet(const TopoDS_Shape &s, const TopTools_ListOfShape &edges, double r1,
                                                double r2) {
  EX_TRY
  BRepFilletAPI_MakeFillet mk(s);
  for (auto it = edges.begin(); it != edges.end(); ++it)
    mk.Add(r1, r2, TopoDS::Edge(*it));
  mk.Build();
  if (!mk.IsDone())
    return ex_null("the fillet could not be built (radius too large?)");
  return ex_wrap(mk.Shape());
  EX_CATCH("fillet")
}

inline std::unique_ptr<TopoDS_Shape> ex_chamfer(const TopoDS_Shape &s, const TopTools_ListOfShape &edges, double d) {
  EX_TRY
  BRepFilletAPI_MakeChamfer mk(s);
  for (auto it = edges.begin(); it != edges.end(); ++it)
    mk.Add(d, TopoDS::Edge(*it));
  mk.Build();
  if (!mk.IsDone())
    return ex_null("the chamfer could not be built (distance too large?)");
  return ex_wrap(mk.Shape());
  EX_CATCH("chamfer")
}

inline std::unique_ptr<TopoDS_Shape> ex_shell(const TopoDS_Shape &s, const TopTools_ListOfShape &faces, double t) {
  EX_TRY
  BRepOffsetAPI_MakeThickSolid mk;
  mk.MakeThickSolidByJoin(s, faces, t, 1e-4, BRepOffset_Skin, Standard_False, Standard_False, GeomAbs_Intersection);
  if (!mk.IsDone())
    return ex_null("shell failed (thickness too large for the part?)");
  return ex_wrap(mk.Shape());
  EX_CATCH("shell")
}

/// The shape with its orientation flipped (makes an inside-out solid right).
inline std::unique_ptr<TopoDS_Shape> ex_reversed(const TopoDS_Shape &s) {
  EX_TRY
  return ex_wrap(s.Reversed());
  EX_CATCH("reverse")
}

/// A closed hollow: the solid with a cavity leaving walls `t` thick inside its surface (t > 0),
/// or with walls `-t` thick grown around it (t < 0, the solid becomes the cavity).
inline std::unique_ptr<TopoDS_Shape> ex_hollow(const TopoDS_Shape &s, double t) {
  EX_TRY
  BRepOffsetAPI_MakeOffsetShape mk;
  mk.PerformByJoin(s, -t, 1e-4, BRepOffset_Skin, Standard_False, Standard_False, GeomAbs_Intersection);
  if (!mk.IsDone())
    return ex_null("shell failed (thickness too large for the part?)");
  TopoDS_Shape off = ex_as_solid(mk.Shape());
  if (ex_solid_volume(off) < 0)
    off.Reverse();
  TopoDS_Shape r = t > 0 ? ex_boolean_raw(s, off, 1, 1e-6) : ex_boolean_raw(off, s, 1, 1e-6);
  if (r.IsNull())
    return ex_null("shell failed (thickness too large for the part?)");
  return ex_wrap(r);
  EX_CATCH("shell")
}

inline std::unique_ptr<TopoDS_Shape> ex_thicken(const TopoDS_Shape &s, double t) {
  EX_TRY
  BRepOffsetAPI_MakeThickSolid mk;
  mk.MakeThickSolidBySimple(s, t);
  if (!mk.IsDone())
    return ex_null("thicken failed");
  TopoDS_Shape r = mk.Shape();
  // the result can come out inside-out depending on the face orientation
  GProp_GProps props;
  BRepGProp::VolumeProperties(r, props);
  if (props.Mass() < 0)
    r.Reverse();
  return ex_wrap(r);
  EX_CATCH("thicken")
}

inline std::unique_ptr<TopoDS_Shape> ex_draft(const TopoDS_Shape &s, const TopTools_ListOfShape &faces, double dx,
                                              double dy, double dz, double angle, double px, double py, double pz,
                                              double nx, double ny, double nz) {
  EX_TRY
  BRepOffsetAPI_DraftAngle mk(s);
  gp_Pln neutral(gp_Pnt(px, py, pz), gp_Dir(nx, ny, nz));
  for (auto it = faces.begin(); it != faces.end(); ++it) {
    mk.Add(TopoDS::Face(*it), gp_Dir(dx, dy, dz), angle, neutral);
    if (!mk.AddDone())
      return ex_null("a face could not be drafted");
  }
  mk.Build();
  if (!mk.IsDone())
    return ex_null("draft failed");
  return ex_wrap(mk.Shape());
  EX_CATCH("draft")
}

// `frenet`: true follows the Frenet trihedron (helices), false the corrected Frenet one, which
// keeps the profile turning with the path also along straight pieces. Sharp corners of the
// path get mitred (right-corner) transitions instead of the profile being dragged round them.
inline std::unique_ptr<TopoDS_Shape> ex_sweep(const TopoDS_Shape &profile_wire, const TopoDS_Shape &spine, bool frenet) {
  EX_TRY
  BRepOffsetAPI_MakePipeShell mk(TopoDS::Wire(spine));
  mk.SetMode(frenet);
  mk.SetTransitionMode(BRepBuilderAPI_RightCorner);
  mk.Add(profile_wire);
  mk.Build();
  if (!mk.IsDone())
    return ex_null("sweep failed");
  mk.MakeSolid();
  return ex_wrap(mk.Shape());
  EX_CATCH("sweep")
}

inline std::unique_ptr<TopoDS_Shape> ex_loft(const TopTools_ListOfShape &wires, bool solid, bool ruled) {
  EX_TRY
  BRepOffsetAPI_ThruSections mk(solid, ruled);
  for (auto it = wires.begin(); it != wires.end(); ++it)
    mk.AddWire(TopoDS::Wire(*it));
  mk.CheckCompatibility(Standard_True);
  mk.Build();
  if (!mk.IsDone())
    return ex_null("loft failed");
  return ex_wrap(mk.Shape());
  EX_CATCH("loft")
}

/// Rigid transform from a row-major 3x4 matrix.
inline std::unique_ptr<TopoDS_Shape> ex_transform(const TopoDS_Shape &s, rust::Slice<const double> m) {
  EX_TRY
  gp_Trsf t;
  t.SetValues(m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8], m[9], m[10], m[11]);
  BRepBuilderAPI_Transform mk(s, t, Standard_True);
  return ex_wrap(mk.Shape());
  EX_CATCH("transform")
}

inline std::unique_ptr<TopoDS_Shape> ex_mirror(const TopoDS_Shape &s, double px, double py, double pz, double nx,
                                               double ny, double nz) {
  EX_TRY
  gp_Trsf t;
  t.SetMirror(gp_Ax2(gp_Pnt(px, py, pz), gp_Dir(nx, ny, nz)));
  BRepBuilderAPI_Transform mk(s, t, Standard_True);
  return ex_wrap(mk.Shape());
  EX_CATCH("mirror")
}

inline std::unique_ptr<TopoDS_Shape> ex_delete_faces(const TopoDS_Shape &s, const TopTools_ListOfShape &faces) {
  EX_TRY
  BRepAlgoAPI_Defeaturing mk;
  mk.SetShape(s);
  mk.AddFacesToRemove(faces);
  mk.SetRunParallel(true);
  mk.Build();
  if (!mk.IsDone() || mk.HasErrors())
    return ex_null("the faces could not be removed and healed");
  return ex_wrap(mk.Shape());
  EX_CATCH("delete face")
}

/// A helix as a wire around axis (o, d), starting at angle 0 of the frame with x direction xd.
inline std::unique_ptr<TopoDS_Shape> ex_helix(double ox, double oy, double oz, double dx, double dy, double dz, double xx,
                                              double xy, double xz, double radius, double pitch, double turns,
                                              bool left) {
  EX_TRY
  gp_Ax3 ax(gp_Pnt(ox, oy, oz), gp_Dir(dx, dy, dz), gp_Dir(xx, xy, xz));
  Handle(Geom_CylindricalSurface) cyl = new Geom_CylindricalSurface(ax, radius);
  double du = left ? -2 * M_PI : 2 * M_PI;
  gp_Lin2d line(gp_Pnt2d(0, 0), gp_Dir2d(du, pitch));
  double len = turns * std::sqrt(du * du + pitch * pitch);
  Handle(Geom2d_TrimmedCurve) seg = GCE2d_MakeSegment(line, 0.0, len);
  TopoDS_Edge e = BRepBuilderAPI_MakeEdge(seg, cyl);
  BRepLib::BuildCurves3d(e);
  return ex_wrap(BRepBuilderAPI_MakeWire(e).Wire());
  EX_CATCH("helix")
}

/// Interpolating B-spline edge through points (xyz triples).
inline std::unique_ptr<TopoDS_Shape> ex_spline(rust::Slice<const double> pts, bool closed) {
  EX_TRY
  int n = (int)(pts.size() / 3);
  if (n < 2)
    return ex_null("a spline needs at least two points");
  Handle(TColgp_HArray1OfPnt) arr = new TColgp_HArray1OfPnt(1, n);
  for (int i = 0; i < n; i++)
    arr->SetValue(i + 1, gp_Pnt(pts[3 * i], pts[3 * i + 1], pts[3 * i + 2]));
  GeomAPI_Interpolate interp(arr, closed, 1e-7);
  interp.Perform();
  if (!interp.IsDone())
    return ex_null("spline interpolation failed");
  return ex_wrap(BRepBuilderAPI_MakeEdge(interp.Curve()).Edge());
  EX_CATCH("spline")
}

/// Ellipse (or elliptical arc from a0 to a1, counter-clockwise about n) as an edge.
inline std::unique_ptr<TopoDS_Shape> ex_ellipse(double cx, double cy, double cz, double nx, double ny, double nz, double xx,
                                                double xy, double xz, double rmaj, double rmin, double a0, double a1,
                                                bool full) {
  EX_TRY
  gp_Elips el(gp_Ax2(gp_Pnt(cx, cy, cz), gp_Dir(nx, ny, nz), gp_Dir(xx, xy, xz)), rmaj, rmin);
  if (full)
    return ex_wrap(BRepBuilderAPI_MakeEdge(el).Edge());
  Handle(Geom_TrimmedCurve) arc = GC_MakeArcOfEllipse(el, a0, a1, Standard_True);
  return ex_wrap(BRepBuilderAPI_MakeEdge(arc).Edge());
  EX_CATCH("ellipse")
}

inline std::unique_ptr<TopoDS_Shape> ex_wire(const TopTools_ListOfShape &edges) {
  EX_TRY
  BRepBuilderAPI_MakeWire mk;
  for (auto it = edges.begin(); it != edges.end(); ++it)
    mk.Add(TopoDS::Edge(*it));
  if (!mk.IsDone())
    return ex_null("the profile edges do not form a connected wire");
  return ex_wrap(mk.Wire());
  EX_CATCH("wire")
}

inline std::unique_ptr<TopoDS_Shape> ex_face(const TopoDS_Shape &outer, const TopTools_ListOfShape &holes) {
  EX_TRY
  BRepBuilderAPI_MakeFace mk(TopoDS::Wire(outer), Standard_True);
  for (auto it = holes.begin(); it != holes.end(); ++it)
    mk.Add(TopoDS::Wire(*it));
  if (!mk.IsDone())
    return ex_null("could not make a face from the profile");
  return ex_fixed(mk.Face());
  EX_CATCH("face")
}

/// Hidden-line projection for drawings. kind: 0 visible sharp, 1 visible smooth, 2 visible outline,
/// 3 hidden sharp, 4 hidden outline. Result lies in the projection's 2D plane (z = 0).
inline std::unique_ptr<TopoDS_Shape> ex_hlr(const TopoDS_Shape &s, double dx, double dy, double dz, double ux, double uy,
                                            double uz, int kind) {
  EX_TRY
  Handle(HLRBRep_Algo) algo = new HLRBRep_Algo();
  algo->Add(s);
  gp_Ax2 ax(gp_Pnt(0, 0, 0), gp_Dir(dx, dy, dz), gp_Dir(ux, uy, uz).Crossed(gp_Dir(dx, dy, dz)));
  algo->Projector(HLRAlgo_Projector(ax));
  algo->Update();
  algo->Hide();
  HLRBRep_HLRToShape out(algo);
  TopoDS_Shape r;
  switch (kind) {
  case 0: r = out.VCompound(); break;
  case 1: r = out.Rg1LineVCompound(); break;
  case 2: r = out.OutLineVCompound(); break;
  case 3: r = out.HCompound(); break;
  default: r = out.OutLineHCompound(); break;
  }
  return ex_wrap(r);
  EX_CATCH("projection")
}

inline bool ex_is_valid(const TopoDS_Shape &s) {
  try {
    return !s.IsNull() && BRepCheck_Analyzer(s).IsValid();
  } catch (...) {
    return false;
  }
}

inline std::unique_ptr<TopoDS_Shape> ex_compound(const TopTools_ListOfShape &shapes) {
  EX_TRY
  BRep_Builder b;
  TopoDS_Compound c;
  b.MakeCompound(c);
  for (auto it = shapes.begin(); it != shapes.end(); ++it)
    b.Add(c, *it);
  return ex_wrap(c);
  EX_CATCH("compound")
}

/// Sew faces/shells into one shell, optionally closing it into a solid.
inline std::unique_ptr<TopoDS_Shape> ex_sew(const TopTools_ListOfShape &shapes, double tol, bool solid) {
  EX_TRY
  BRepBuilderAPI_Sewing sew(tol);
  for (auto it = shapes.begin(); it != shapes.end(); ++it)
    sew.Add(*it);
  sew.Perform();
  TopoDS_Shape r = sew.SewedShape();
  if (r.IsNull())
    return ex_null("the surfaces could not be knitted");
  if (solid) {
    BRepBuilderAPI_MakeSolid ms;
    bool any = false;
    for (TopExp_Explorer ex(r, TopAbs_SHELL); ex.More(); ex.Next()) {
      ms.Add(TopoDS::Shell(ex.Current()));
      any = true;
    }
    if (!any || !ms.IsDone())
      return ex_null("the knitted surfaces do not close a volume");
    TopoDS_Shape so = ms.Shape();
    GProp_GProps props;
    BRepGProp::VolumeProperties(so, props);
    if (props.Mass() < 0)
      so.Reverse();
    return ex_wrap(so);
  }
  return ex_wrap(r);
  EX_CATCH("knit")
}

/// A surface patch filling a closed boundary of edges.
inline std::unique_ptr<TopoDS_Shape> ex_fill(const TopTools_ListOfShape &edges) {
  EX_TRY
  BRepOffsetAPI_MakeFilling mk;
  for (auto it = edges.begin(); it != edges.end(); ++it)
    mk.Add(TopoDS::Edge(*it), GeomAbs_C0);
  mk.Build();
  if (!mk.IsDone())
    return ex_null("the boundary could not be filled (is it closed?)");
  return ex_wrap(mk.Shape());
  EX_CATCH("fill")
}

/// Offset a surface (or every face of a shape) by a distance.
inline std::unique_ptr<TopoDS_Shape> ex_offset_shape(const TopoDS_Shape &s, double d) {
  EX_TRY
  BRepOffsetAPI_MakeOffsetShape mk;
  mk.PerformBySimple(s, d);
  if (!mk.IsDone())
    return ex_null("offset failed");
  return ex_wrap(mk.Shape());
  EX_CATCH("offset")
}

/// Chamfer with two distances (mode 0) or a distance and an angle in radians (mode 1),
/// measured on the first face next to each edge.
inline std::unique_ptr<TopoDS_Shape> ex_chamfer2(const TopoDS_Shape &s, const TopTools_ListOfShape &edges, double d1, double d2, int mode) {
  EX_TRY
  TopTools_IndexedDataMapOfShapeListOfShape map;
  TopExp::MapShapesAndAncestors(s, TopAbs_EDGE, TopAbs_FACE, map);
  BRepFilletAPI_MakeChamfer mk(s);
  for (auto it = edges.begin(); it != edges.end(); ++it) {
    const TopoDS_Edge &e = TopoDS::Edge(*it);
    if (!map.Contains(e) || map.FindFromKey(e).IsEmpty())
      return ex_null("an edge is not on the part");
    const TopoDS_Face &f = TopoDS::Face(map.FindFromKey(e).First());
    if (mode == 0)
      mk.Add(d1, d2, e, f);
    else
      mk.AddDA(d1, d2, e, f);
  }
  mk.Build();
  if (!mk.IsDone())
    return ex_null("the chamfer could not be built (too large?)");
  return ex_wrap(mk.Shape());
  EX_CATCH("chamfer")
}

/// Move faces of a solid along their normals, keeping the rest in place.
inline std::unique_ptr<TopoDS_Shape> ex_offset_faces(const TopoDS_Shape &s, const TopTools_ListOfShape &faces, double d) {
  EX_TRY
  BRepOffset_MakeOffset mo;
  mo.Initialize(s, 0.0, 1e-5, BRepOffset_Skin, Standard_True, Standard_False, GeomAbs_Intersection, Standard_False, Standard_False);
  for (auto it = faces.begin(); it != faces.end(); ++it)
    mo.SetOffsetOnFace(TopoDS::Face(*it), d);
  mo.MakeOffsetShape();
  if (!mo.IsDone())
    return ex_null("the faces could not be moved");
  return ex_wrap(ex_unify_raw(mo.Shape()));
  EX_CATCH("move face")
}

// ---------------------------------------------------------------- scripting queries
// Used by the headless scripting API (src/script_api.rs). Queries return empty vectors
// on failure (with ex_last_error() set) rather than throwing.

#include <BRepAdaptor_Curve.hxx>
#include <BRepAdaptor_Surface.hxx>
#include <BRepClass3d_SolidClassifier.hxx>
#include <BRepClass_FaceClassifier.hxx>
#include <BRepOffsetAPI_MakeOffset.hxx>
#include <GCPnts_QuasiUniformDeflection.hxx>
#include <GeomAPI_ProjectPointOnSurf.hxx>
#include <GeomLProp_SLProps.hxx>
#include <ShapeFix_Face.hxx>
#include <ShapeAnalysis_Surface.hxx>
#include <BRepAlgoAPI_BooleanOperation.hxx>
#include <gp_Circ.hxx>
#include <thread>
#include <atomic>
#include <algorithm>
#include <functional>

#define EX_QTRY try {
#define EX_QCATCH(what, empty)                                                                                         \
  }                                                                                                                    \
  catch (const Standard_Failure &e) {                                                                                  \
    ex_err_slot() = std::string(what) + ": " + e.GetMessageString();                                                   \
    return empty;                                                                                                      \
  }                                                                                                                    \
  catch (...) {                                                                                                        \
    ex_err_slot() = std::string(what) + " failed";                                                                     \
    return empty;                                                                                                      \
  }

/// Mass properties: kind 0 volume, 1 area, 2 length. [mass, cx, cy, cz, I (3×3 row-major, about the centre)].
inline rust::Vec<double> ex_mass_props(const TopoDS_Shape &s, int kind) {
  rust::Vec<double> out;
  EX_QTRY
  GProp_GProps g;
  if (kind == 0)
    BRepGProp::VolumeProperties(s, g);
  else if (kind == 1)
    BRepGProp::SurfaceProperties(s, g);
  else
    BRepGProp::LinearProperties(s, g);
  out.push_back(g.Mass());
  gp_Pnt c = g.CentreOfMass();
  out.push_back(c.X());
  out.push_back(c.Y());
  out.push_back(c.Z());
  gp_Mat m = g.MatrixOfInertia();
  for (int i = 1; i <= 3; i++)
    for (int j = 1; j <= 3; j++)
      out.push_back(m(i, j));
  return out;
  EX_QCATCH("mass properties", rust::Vec<double>())
}

/// Classify points (xyz…) against a solid: 0 inside, 1 outside, 2 on the boundary (within tol), 3 unknown.
/// Runs on several threads, each with its own classifier.
inline rust::Vec<int32_t> ex_classify(const TopoDS_Shape &s, rust::Slice<const double> pts, double tol) {
  rust::Vec<int32_t> out;
  size_t n = pts.size() / 3;
  std::vector<int32_t> res(n, 3);
  EX_QTRY
  unsigned hw = std::max(1u, std::min(16u, std::thread::hardware_concurrency()));
  size_t nt = n < 64 ? 1 : std::min<size_t>(hw, n / 32);
  auto work = [&](size_t a, size_t b) {
    try {
      BRepClass3d_SolidClassifier cl(s);
      for (size_t i = a; i < b; i++) {
        cl.Perform(gp_Pnt(pts[3 * i], pts[3 * i + 1], pts[3 * i + 2]), tol);
        TopAbs_State st = cl.State();
        res[i] = st == TopAbs_IN ? 0 : st == TopAbs_OUT ? 1 : st == TopAbs_ON ? 2 : 3;
      }
    } catch (...) {
    }
  };
  if (nt <= 1) {
    work(0, n);
  } else {
    std::vector<std::thread> th;
    size_t chunk = (n + nt - 1) / nt;
    for (size_t t = 0; t < nt; t++) {
      size_t a = t * chunk, b = std::min(n, a + chunk);
      if (a < b)
        th.emplace_back(work, a, b);
    }
    for (auto &t : th)
      t.join();
  }
  for (auto v : res)
    out.push_back(v);
  return out;
  EX_QCATCH("classify", rust::Vec<int32_t>())
}

/// Number of distinct sub-shapes of a kind (0 solid, 1 shell, 2 face, 3 wire, 4 edge, 5 vertex).
inline TopAbs_ShapeEnum ex_kind(int kind) {
  switch (kind) {
  case 0: return TopAbs_SOLID;
  case 1: return TopAbs_SHELL;
  case 2: return TopAbs_FACE;
  case 3: return TopAbs_WIRE;
  case 4: return TopAbs_EDGE;
  default: return TopAbs_VERTEX;
  }
}

inline int32_t ex_count(const TopoDS_Shape &s, int kind) {
  TopTools_IndexedMapOfShape m;
  TopExp::MapShapes(s, ex_kind(kind), m);
  return m.Extent();
}

/// The i-th (from 0) distinct sub-shape of a kind, in a stable order.
inline std::unique_ptr<TopoDS_Shape> ex_sub(const TopoDS_Shape &s, int kind, int i) {
  TopTools_IndexedMapOfShape m;
  TopExp::MapShapes(s, ex_kind(kind), m);
  if (i < 0 || i >= m.Extent())
    return ex_null("no such sub-shape");
  return ex_wrap(m(i + 1));
}

/// For each edge (ex_sub order): the number of faces around it, then their indices (ex_sub order).
inline rust::Vec<int32_t> ex_edge_faces(const TopoDS_Shape &s) {
  rust::Vec<int32_t> out;
  EX_QTRY
  TopTools_IndexedMapOfShape em, fm;
  TopExp::MapShapes(s, TopAbs_EDGE, em);
  TopExp::MapShapes(s, TopAbs_FACE, fm);
  TopTools_IndexedDataMapOfShapeListOfShape map;
  TopExp::MapShapesAndAncestors(s, TopAbs_EDGE, TopAbs_FACE, map);
  for (int i = 1; i <= em.Extent(); i++) {
    std::vector<int32_t> ids;
    if (map.Contains(em(i))) {
      for (auto it = map.FindFromKey(em(i)).begin(); it != map.FindFromKey(em(i)).end(); ++it) {
        int k = fm.FindIndex(*it) - 1;
        if (k >= 0 && std::find(ids.begin(), ids.end(), k) == ids.end())
          ids.push_back(k);
      }
    }
    out.push_back((int32_t)ids.size());
    for (auto k : ids)
      out.push_back(k);
  }
  return out;
  EX_QCATCH("edge faces", rust::Vec<int32_t>())
}

/// A face at the point nearest (x, y, z): [surface type (GeomAbs_SurfaceType), normal xyz (outward,
/// following the face's orientation), nearest point xyz, plus for planes/cylinders/cones/spheres the
/// axis or plane: location xyz, direction xyz, radius].
inline rust::Vec<double> ex_face_at(const TopoDS_Shape &f, double x, double y, double z) {
  rust::Vec<double> out;
  EX_QTRY
  const TopoDS_Face &face = TopoDS::Face(f);
  BRepAdaptor_Surface ad(face);
  Handle(Geom_Surface) surf = BRep_Tool::Surface(face);
  ShapeAnalysis_Surface sas(surf);
  gp_Pnt2d uv = sas.ValueOfUV(gp_Pnt(x, y, z), 1e-7);
  GeomLProp_SLProps props(surf, uv.X(), uv.Y(), 1, 1e-9);
  gp_Dir n(0, 0, 1);
  if (props.IsNormalDefined())
    n = props.Normal();
  if (face.Orientation() == TopAbs_REVERSED)
    n.Reverse();
  gp_Pnt q = surf->Value(uv.X(), uv.Y());
  out.push_back((double)ad.GetType());
  out.push_back(n.X()); out.push_back(n.Y()); out.push_back(n.Z());
  out.push_back(q.X()); out.push_back(q.Y()); out.push_back(q.Z());
  gp_Ax1 ax;
  double r = 0.0;
  bool has = true;
  switch (ad.GetType()) {
  case GeomAbs_Plane: ax = ad.Plane().Axis(); break;
  case GeomAbs_Cylinder: ax = ad.Cylinder().Axis(); r = ad.Cylinder().Radius(); break;
  case GeomAbs_Cone: ax = ad.Cone().Axis(); r = ad.Cone().RefRadius(); break;
  case GeomAbs_Sphere: ax = ad.Sphere().Position().Axis(); r = ad.Sphere().Radius(); break;
  case GeomAbs_Torus: ax = ad.Torus().Axis(); r = ad.Torus().MajorRadius(); break;
  default: has = false;
  }
  if (has) {
    out.push_back(ax.Location().X()); out.push_back(ax.Location().Y()); out.push_back(ax.Location().Z());
    out.push_back(ax.Direction().X()); out.push_back(ax.Direction().Y()); out.push_back(ax.Direction().Z());
    out.push_back(r);
  }
  return out;
  EX_QCATCH("face query", rust::Vec<double>())
}

/// An edge: [curve type (GeomAbs_CurveType), length, start xyz, middle xyz, end xyz, plus for circles
/// centre xyz, axis xyz, radius].
inline rust::Vec<double> ex_edge_info(const TopoDS_Shape &e) {
  rust::Vec<double> out;
  EX_QTRY
  const TopoDS_Edge &edge = TopoDS::Edge(e);
  BRepAdaptor_Curve c(edge);
  GProp_GProps g;
  BRepGProp::LinearProperties(edge, g);
  double a = c.FirstParameter(), b = c.LastParameter();
  out.push_back((double)c.GetType());
  out.push_back(g.Mass());
  for (double t : {a, 0.5 * (a + b), b}) {
    gp_Pnt p = c.Value(t);
    out.push_back(p.X()); out.push_back(p.Y()); out.push_back(p.Z());
  }
  if (c.GetType() == GeomAbs_Circle) {
    gp_Circ ci = c.Circle();
    out.push_back(ci.Location().X()); out.push_back(ci.Location().Y()); out.push_back(ci.Location().Z());
    out.push_back(ci.Axis().Direction().X()); out.push_back(ci.Axis().Direction().Y()); out.push_back(ci.Axis().Direction().Z());
    out.push_back(ci.Radius());
  }
  return out;
  EX_QCATCH("edge query", rust::Vec<double>())
}

/// Points along an edge, no further than `deflection` from it.
inline rust::Vec<double> ex_edge_points(const TopoDS_Shape &e, double deflection) {
  rust::Vec<double> out;
  EX_QTRY
  BRepAdaptor_Curve c(TopoDS::Edge(e));
  GCPnts_QuasiUniformDeflection d(c, std::max(deflection, 1e-6));
  if (!d.IsDone())
    return out;
  for (int i = 1; i <= d.NbPoints(); i++) {
    gp_Pnt p = d.Value(i);
    out.push_back(p.X()); out.push_back(p.Y()); out.push_back(p.Z());
  }
  return out;
  EX_QCATCH("edge points", rust::Vec<double>())
}

inline bool ex_is_same(const TopoDS_Shape &a, const TopoDS_Shape &b) { return a.IsSame(b); }

/// [xmin, ymin, zmin, xmax, ymax, zmax]; `optimal` uses the exact geometry instead of the tolerant box.
inline rust::Vec<double> ex_bbox(const TopoDS_Shape &s, bool optimal) {
  rust::Vec<double> out;
  EX_QTRY
  Bnd_Box b;
  if (optimal)
    BRepBndLib::AddOptimal(s, b, Standard_False, Standard_False);
  else
    BRepBndLib::Add(s, b);
  if (b.IsVoid())
    return out;
  double x0, y0, z0, x1, y1, z1;
  b.Get(x0, y0, z0, x1, y1, z1);
  for (double v : {x0, y0, z0, x1, y1, z1})
    out.push_back(v);
  return out;
  EX_QCATCH("bounding box", rust::Vec<double>())
}

/// Boolean of several arguments with several tools: op 0 fuse, 1 cut, 2 common. `fuzzy` treats gaps
/// smaller than it as touching; `unify` merges coplanar faces afterwards (remove splitter).
inline std::unique_ptr<TopoDS_Shape> ex_boolean_n(const TopTools_ListOfShape &args, const TopTools_ListOfShape &tools, int op, double fuzzy, bool unify) {
  EX_TRY
  std::unique_ptr<BRepAlgoAPI_BooleanOperation> bop;
  if (op == 0)
    bop.reset(new BRepAlgoAPI_Fuse());
  else if (op == 1)
    bop.reset(new BRepAlgoAPI_Cut());
  else
    bop.reset(new BRepAlgoAPI_Common());
  bop->SetArguments(args);
  bop->SetTools(tools);
  if (fuzzy > 0.0)
    bop->SetFuzzyValue(fuzzy);
  bop->SetRunParallel(true);
  bop->Build();
  if (!bop->IsDone() || bop->HasErrors())
    return ex_null(op == 0 ? "fuse failed" : op == 1 ? "cut failed" : "common failed");
  return ex_wrap(unify ? ex_unify_raw(bop->Shape()) : bop->Shape());
  EX_CATCH("boolean")
}

/// Planar faces, each with its holes, from closed wires lying in one plane: a wire inside an odd
/// number of others is a hole of the smallest one around it.
inline TopoDS_Shape ex_faces_from_wires(const std::vector<TopoDS_Wire> &wires, const gp_Pln &pln) {
  size_t n = wires.size();
  std::vector<TopoDS_Face> plain;
  std::vector<double> area;
  // each wire's own face (Inside: turned so that the face is the finite side)
  for (auto &w : wires) {
    TopoDS_Face f = BRepBuilderAPI_MakeFace(pln, w, Standard_True).Face();
    GProp_GProps g;
    BRepGProp::SurfaceProperties(f, g);
    plain.push_back(f);
    area.push_back(std::abs(g.Mass()));
  }
  // containment: is wire i inside wire j?
  auto inside = [&](size_t i, size_t j) {
    TopExp_Explorer ex(wires[i], TopAbs_VERTEX);
    if (!ex.More())
      return false;
    gp_Pnt p = BRep_Tool::Pnt(TopoDS::Vertex(ex.Current()));
    BRepClass_FaceClassifier fc(plain[j], p, 1e-7);
    return area[j] > area[i] && fc.State() == TopAbs_IN;
  };
  std::vector<int> depth(n, 0), parent(n, -1);
  for (size_t i = 0; i < n; i++)
    for (size_t j = 0; j < n; j++)
      if (i != j && inside(i, j)) {
        depth[i]++;
        if (parent[i] < 0 || area[j] < area[parent[i]])
          parent[i] = (int)j;
      }
  // signed area of a wire's face on the plane: positive when it runs counter-clockwise about the
  // normal (Inside false, so that the wire is not turned)
  auto signed_area = [&](const TopoDS_Wire &w) {
    GProp_GProps g;
    BRepGProp::SurfaceProperties(BRepBuilderAPI_MakeFace(pln, w, Standard_False).Face(), g);
    return g.Mass();
  };
  BRep_Builder bb;
  TopoDS_Compound comp;
  bb.MakeCompound(comp);
  for (size_t i = 0; i < n; i++) {
    if (depth[i] % 2)
      continue;
    TopoDS_Wire outer = signed_area(wires[i]) < 0 ? TopoDS::Wire(wires[i].Reversed()) : wires[i];
    BRepBuilderAPI_MakeFace mf(pln, outer, Standard_True);
    for (size_t k = 0; k < n; k++)
      if (depth[k] % 2 && parent[k] == (int)i)
        mf.Add(signed_area(wires[k]) > 0 ? TopoDS::Wire(wires[k].Reversed()) : wires[k]);
    bb.Add(comp, mf.Face());
  }
  return comp;
}

/// 2D offset of a planar face (with its holes): outward for d > 0, inward for d < 0, corners joined
/// with arcs (`arc`) or by extending the sides. The result is a compound of faces (empty when it vanishes).
inline std::unique_ptr<TopoDS_Shape> ex_offset2d(const TopoDS_Shape &f, double d, bool arc) {
  EX_TRY
  const TopoDS_Face &face = TopoDS::Face(f);
  BRepAdaptor_Surface ad(face);
  if (ad.GetType() != GeomAbs_Plane)
    return ex_null("2D offset needs a planar face");
  gp_Pln pln = ad.Plane();
  BRepOffsetAPI_MakeOffset mk(face, arc ? GeomAbs_Arc : GeomAbs_Intersection);
  mk.Perform(d);
  BRep_Builder bb;
  TopoDS_Compound empty;
  bb.MakeCompound(empty);
  if (!mk.IsDone())
    return ex_wrap(empty);
  std::vector<TopoDS_Wire> wires;
  for (TopExp_Explorer ex(mk.Shape(), TopAbs_WIRE); ex.More(); ex.Next())
    if (BRep_Tool::IsClosed(ex.Current()))
      wires.push_back(TopoDS::Wire(ex.Current()));
  if (wires.empty())
    return ex_wrap(empty);
  return ex_wrap(ex_faces_from_wires(wires, pln));
  EX_CATCH("2D offset")
}

/// The same face turned to face along (nx, ny, nz).
inline std::unique_ptr<TopoDS_Shape> ex_face_towards(const TopoDS_Shape &f, double nx, double ny, double nz) {
  EX_TRY
  BRepAdaptor_Surface ad(TopoDS::Face(f));
  double u = 0.5 * (ad.FirstUParameter() + ad.LastUParameter()), v = 0.5 * (ad.FirstVParameter() + ad.LastVParameter());
  GeomLProp_SLProps props(BRep_Tool::Surface(TopoDS::Face(f)), u, v, 1, 1e-9);
  if (!props.IsNormalDefined())
    return ex_wrap(f);
  gp_Dir n = props.Normal();
  if (f.Orientation() == TopAbs_REVERSED)
    n.Reverse();
  if (n.X() * nx + n.Y() * ny + n.Z() * nz < 0)
    return ex_wrap(f.Reversed());
  return ex_wrap(f);
  EX_CATCH("orient face")
}

// ---------------------------------------------------------------- scripting from Python
// Primitives placed on an axis, sub-shape maps, sections, distances and the queries a
// FreeCAD-style Python API needs.
#include <BRepPrimAPI_MakeBox.hxx>
#include <BRepPrimAPI_MakeCylinder.hxx>
#include <BRepPrimAPI_MakeCone.hxx>
#include <BRepPrimAPI_MakeSphere.hxx>
#include <BRepBuilderAPI_MakePolygon.hxx>
#include <BRepBuilderAPI_Copy.hxx>
#include <BRepBuilderAPI_GTransform.hxx>
#include <BRepAlgoAPI_Section.hxx>
#include <BRepExtrema_DistShapeShape.hxx>
#include <BRepTopAdaptor_FClass2d.hxx>
#include <GC_MakeArcOfCircle.hxx>
#include <GC_MakeSegment.hxx>
#include <GCPnts_UniformAbscissa.hxx>
#include <GCPnts_UniformDeflection.hxx>
#include <ShapeAnalysis_FreeBounds.hxx>
#include <TopTools_HSequenceOfShape.hxx>
#include <StlAPI_Writer.hxx>
#include <TopLoc_Location.hxx>
#include <gp_GTrsf.hxx>
#include <Poly.hxx>
#include <BRepLib_FindSurface.hxx>
#include <TopoDS_Iterator.hxx>
#include <Precision.hxx>
#include <gp_Pnt2d.hxx>
#include <BRepAdaptor_CompCurve.hxx>
#include <Adaptor3d_Curve.hxx>

inline gp_Ax2 ex_ax2(double px, double py, double pz, double dx, double dy, double dz) {
  return gp_Ax2(gp_Pnt(px, py, pz), gp_Dir(dx, dy, dz));
}

/// Box of l × w × h with its corner at p, on the axis system gp_Ax2(p, d) (FreeCAD's makeBox).
inline std::unique_ptr<TopoDS_Shape> ex_make_box(double px, double py, double pz, double dx, double dy, double dz, double l,
                                                 double w, double h) {
  EX_TRY
  if (l < 1e-7 || w < 1e-7 || h < 1e-7)
    return ex_null("box: every side must be longer than zero");
  return ex_wrap(BRepPrimAPI_MakeBox(ex_ax2(px, py, pz, dx, dy, dz), l, w, h).Shape());
  EX_CATCH("box")
}

/// Cylinder of radius r and height h on the axis (p, d), swept through `angle` degrees.
inline std::unique_ptr<TopoDS_Shape> ex_make_cylinder(double px, double py, double pz, double dx, double dy, double dz,
                                                      double r, double h, double angle) {
  EX_TRY
  if (r < 1e-7 || h < 1e-7)
    return ex_null("cylinder: radius and height must be larger than zero");
  return ex_wrap(BRepPrimAPI_MakeCylinder(ex_ax2(px, py, pz, dx, dy, dz), r, h, angle * M_PI / 180.0).Shape());
  EX_CATCH("cylinder")
}

inline std::unique_ptr<TopoDS_Shape> ex_make_cone(double px, double py, double pz, double dx, double dy, double dz, double r1,
                                                  double r2, double h, double angle) {
  EX_TRY
  if (h < 1e-7 || (r1 < 1e-7 && r2 < 1e-7))
    return ex_null("cone: height and one radius must be larger than zero");
  return ex_wrap(BRepPrimAPI_MakeCone(ex_ax2(px, py, pz, dx, dy, dz), r1, r2, h, angle * M_PI / 180.0).Shape());
  EX_CATCH("cone")
}

/// Sphere of radius r about p; a1, a2 the latitudes and a3 the longitude swept (degrees).
inline std::unique_ptr<TopoDS_Shape> ex_make_sphere(double px, double py, double pz, double dx, double dy, double dz, double r,
                                                    double a1, double a2, double a3) {
  EX_TRY
  if (r < 1e-7)
    return ex_null("sphere: radius must be larger than zero");
  double k = M_PI / 180.0;
  return ex_wrap(BRepPrimAPI_MakeSphere(ex_ax2(px, py, pz, dx, dy, dz), r, a1 * k, a2 * k, a3 * k).Shape());
  EX_CATCH("sphere")
}

inline std::unique_ptr<TopoDS_Shape> ex_vertex(double x, double y, double z) {
  EX_TRY
  return ex_wrap(BRepBuilderAPI_MakeVertex(gp_Pnt(x, y, z)).Vertex());
  EX_CATCH("vertex")
}

/// Polyline wire through the points (xyz…); `close` joins the last point to the first. A last point on the
/// first closes it too.
inline std::unique_ptr<TopoDS_Shape> ex_polygon(rust::Slice<const double> pts, bool close) {
  EX_TRY
  BRepBuilderAPI_MakePolygon mk;
  size_t n = pts.size() / 3;
  // a last point on the first closes the wire (one shared vertex)
  if (n > 2 && gp_Pnt(pts[0], pts[1], pts[2]).Distance(gp_Pnt(pts[3 * n - 3], pts[3 * n - 2], pts[3 * n - 1])) <= Precision::Confusion()) {
    n--;
    close = true;
  }
  for (size_t i = 0; i < n; i++)
    mk.Add(gp_Pnt(pts[3 * i], pts[3 * i + 1], pts[3 * i + 2]));
  if (close)
    mk.Close();
  if (!mk.IsDone())
    return ex_null("polygon: needs at least two distinct points");
  return ex_wrap(mk.Wire());
  EX_CATCH("polygon")
}

inline std::unique_ptr<TopoDS_Shape> ex_segment(double ax, double ay, double az, double bx, double by, double bz) {
  EX_TRY
  GC_MakeSegment mk(gp_Pnt(ax, ay, az), gp_Pnt(bx, by, bz));
  if (!mk.IsDone())
    return ex_null("line segment: the points coincide");
  return ex_wrap(BRepBuilderAPI_MakeEdge(mk.Value()).Edge());
  EX_CATCH("line segment")
}

/// Circular arc through three points.
inline std::unique_ptr<TopoDS_Shape> ex_arc3(rust::Slice<const double> p) {
  EX_TRY
  GC_MakeArcOfCircle mk(gp_Pnt(p[0], p[1], p[2]), gp_Pnt(p[3], p[4], p[5]), gp_Pnt(p[6], p[7], p[8]));
  if (!mk.IsDone())
    return ex_null("arc: the points are in a line");
  return ex_wrap(BRepBuilderAPI_MakeEdge(mk.Value()).Edge());
  EX_CATCH("arc")
}

/// Wire from edges and wires, in order.
inline std::unique_ptr<TopoDS_Shape> ex_wire_from(const TopTools_ListOfShape &parts) {
  EX_TRY
  BRepBuilderAPI_MakeWire mk;
  for (auto it = parts.begin(); it != parts.end(); ++it) {
    if (it->ShapeType() == TopAbs_WIRE)
      mk.Add(TopoDS::Wire(*it));
    else if (it->ShapeType() == TopAbs_EDGE)
      mk.Add(TopoDS::Edge(*it));
    else
      return ex_null("wire: give edges or wires");
  }
  if (!mk.IsDone())
    return ex_null("wire: the edges do not connect");
  return ex_wrap(mk.Wire());
  EX_CATCH("wire")
}

/// Face bounded by a closed wire (holes: further wires inside it), on the surface the wire lies on.
inline std::unique_ptr<TopoDS_Shape> ex_face_from_wire(const TopoDS_Shape &outer, const TopTools_ListOfShape &holes) {
  EX_TRY
  TopoDS_Shape w = outer;
  if (w.ShapeType() == TopAbs_EDGE)
    w = BRepBuilderAPI_MakeWire(TopoDS::Edge(w)).Wire();
  if (w.ShapeType() != TopAbs_WIRE)
    return ex_null("face: give a closed wire");
  BRepBuilderAPI_MakeFace mk(TopoDS::Wire(w));
  for (auto it = holes.begin(); it != holes.end(); ++it)
    mk.Add(TopoDS::Wire(*it));
  if (!mk.IsDone())
    return ex_null("face: could not make a face from the wire");
  return ex_wrap(mk.Face());
  EX_CATCH("face")
}

/// Every distinct sub-shape of a kind (ex_kind numbering), in TopExp::MapShapes order.
inline std::unique_ptr<TopTools_IndexedMapOfShape> ex_map(const TopoDS_Shape &s, int kind) {
  std::unique_ptr<TopTools_IndexedMapOfShape> m(new TopTools_IndexedMapOfShape());
  try {
    if (!s.IsNull())
      TopExp::MapShapes(s, ex_kind(kind), *m);
  } catch (...) {
  }
  return m;
}

/// The shapes directly inside a compound (or any shape), in order.
inline std::unique_ptr<TopTools_IndexedMapOfShape> ex_children(const TopoDS_Shape &s) {
  std::unique_ptr<TopTools_IndexedMapOfShape> m(new TopTools_IndexedMapOfShape());
  if (!s.IsNull())
    for (TopoDS_Iterator it(s); it.More(); it.Next())
      m->Add(it.Value());
  return m;
}

/// TopAbs_ShapeEnum of the shape (0 compound … 7 vertex), -1 when null.
inline int32_t ex_shape_type(const TopoDS_Shape &s) { return s.IsNull() ? -1 : (int32_t)s.ShapeType(); }

inline std::unique_ptr<TopoDS_Shape> ex_copy(const TopoDS_Shape &s) {
  EX_TRY
  return ex_wrap(BRepBuilderAPI_Copy(s).Shape());
  EX_CATCH("copy")
}

/// The shape moved by a rigid transform (row-major 3 × 4), sharing its geometry (a new location).
inline std::unique_ptr<TopoDS_Shape> ex_moved(const TopoDS_Shape &s, rust::Slice<const double> m) {
  EX_TRY
  gp_Trsf t;
  t.SetValues(m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8], m[9], m[10], m[11]);
  return ex_wrap(s.Moved(TopLoc_Location(t)));
  EX_CATCH("move")
}

/// A general affine transform (row-major 3 × 4), scaling or shearing allowed: the geometry is converted.
inline std::unique_ptr<TopoDS_Shape> ex_gtransform(const TopoDS_Shape &s, rust::Slice<const double> m) {
  EX_TRY
  gp_GTrsf g;
  for (int i = 0; i < 3; i++) {
    for (int j = 0; j < 3; j++)
      g.SetValue(i + 1, j + 1, m[4 * i + j]);
    g.SetValue(i + 1, 4, m[4 * i + 3]);
  }
  BRepBuilderAPI_GTransform mk(s, g, Standard_True);
  return ex_wrap(mk.Shape());
  EX_CATCH("transform")
}

/// Bounding box as FreeCAD gives it (BRepBndLib::Add, no gap): [xmin, ymin, zmin, xmax, ymax, zmax].
inline rust::Vec<double> ex_bbox_tight(const TopoDS_Shape &s) {
  rust::Vec<double> out;
  EX_QTRY
  Bnd_Box b;
  BRepBndLib::Add(s, b);
  b.SetGap(0.0);
  if (b.IsVoid())
    return out;
  double x0, y0, z0, x1, y1, z1;
  b.Get(x0, y0, z0, x1, y1, z1);
  for (double v : {x0, y0, z0, x1, y1, z1})
    out.push_back(v);
  return out;
  EX_QCATCH("bounding box", rust::Vec<double>())
}

inline rust::Vec<double> ex_vertex_point(const TopoDS_Shape &v) {
  rust::Vec<double> out;
  EX_QTRY
  gp_Pnt p = BRep_Tool::Pnt(TopoDS::Vertex(v));
  out.push_back(p.X()); out.push_back(p.Y()); out.push_back(p.Z());
  return out;
  EX_QCATCH("vertex", rust::Vec<double>())
}

/// Minimum distance between two shapes: [distance, then for each solution the point on a and on b (6 values)].
inline rust::Vec<double> ex_distance(const TopoDS_Shape &a, const TopoDS_Shape &b) {
  rust::Vec<double> out;
  EX_QTRY
  BRepExtrema_DistShapeShape d(a, b);
  if (!d.IsDone() || d.NbSolution() < 1) {
    ex_err_slot() = "distance: no solution";
    return out;
  }
  out.push_back(d.Value());
  for (int i = 1; i <= d.NbSolution(); i++) {
    gp_Pnt p = d.PointOnShape1(i), q = d.PointOnShape2(i);
    for (double v : {p.X(), p.Y(), p.Z(), q.X(), q.Y(), q.Z()})
      out.push_back(v);
  }
  return out;
  EX_QCATCH("distance", rust::Vec<double>())
}

/// Section of a shape with the plane n·x = d: the cut edges joined into wires (a compound of wires).
inline std::unique_ptr<TopoDS_Shape> ex_slice(const TopoDS_Shape &s, double nx, double ny, double nz, double d) {
  EX_TRY
  gp_Dir n(nx, ny, nz);
  gp_Pln pln(n.X(), n.Y(), n.Z(), -d);
  BRepAlgoAPI_Section sec(s, pln, Standard_False);
  sec.ComputePCurveOn1(Standard_True);
  sec.Approximation(Standard_True);
  sec.Build();
  if (!sec.IsDone())
    return ex_null("slice failed");
  Handle(TopTools_HSequenceOfShape) edges = new TopTools_HSequenceOfShape();
  for (TopExp_Explorer ex(sec.Shape(), TopAbs_EDGE); ex.More(); ex.Next())
    edges->Append(ex.Current());
  Handle(TopTools_HSequenceOfShape) wires = new TopTools_HSequenceOfShape();
  ShapeAnalysis_FreeBounds::ConnectEdgesToWires(edges, Precision::Confusion(), Standard_False, wires);
  BRep_Builder bb;
  TopoDS_Compound comp;
  bb.MakeCompound(comp);
  for (int i = 1; i <= wires->Length(); i++)
    bb.Add(comp, wires->Value(i));
  return ex_wrap(comp);
  EX_CATCH("slice")
}

/// Points along an edge or wire: mode 0 `value` points evenly spaced by length, mode 1 no further than `value` from
/// the curve (uniform deflection), mode 2 quasi-uniform deflection.
inline rust::Vec<double> ex_discretize(const TopoDS_Shape &e, int mode, double value) {
  rust::Vec<double> out;
  EX_QTRY
  std::unique_ptr<Adaptor3d_Curve> cp;
  if (e.ShapeType() == TopAbs_WIRE)
    cp.reset(new BRepAdaptor_CompCurve(TopoDS::Wire(e)));
  else
    cp.reset(new BRepAdaptor_Curve(TopoDS::Edge(e)));
  Adaptor3d_Curve &c = *cp;
  auto put = [&](const gp_Pnt &p) { out.push_back(p.X()); out.push_back(p.Y()); out.push_back(p.Z()); };
  if (mode == 0) {
    GCPnts_UniformAbscissa d(c, std::max(2, (int)value));
    if (!d.IsDone())
      return out;
    for (int i = 1; i <= d.NbPoints(); i++)
      put(c.Value(d.Parameter(i)));
  } else if (mode == 1) {
    GCPnts_UniformDeflection d(c, std::max(value, 1e-7));
    if (!d.IsDone())
      return out;
    for (int i = 1; i <= d.NbPoints(); i++)
      put(d.Value(i));
  } else {
    GCPnts_QuasiUniformDeflection d(c, std::max(value, 1e-7));
    if (!d.IsDone())
      return out;
    for (int i = 1; i <= d.NbPoints(); i++)
      put(d.Value(i));
  }
  return out;
  EX_QCATCH("discretize", rust::Vec<double>())
}

/// (u, v) of the point on the face's surface nearest p.
inline rust::Vec<double> ex_surface_uv(const TopoDS_Shape &f, double x, double y, double z) {
  rust::Vec<double> out;
  EX_QTRY
  Handle(Geom_Surface) surf = BRep_Tool::Surface(TopoDS::Face(f));
  GeomAPI_ProjectPointOnSurf pr(gp_Pnt(x, y, z), surf);
  if (pr.NbPoints() < 1) {
    ex_err_slot() = "surface parameter: projection failed";
    return out;
  }
  double u, v;
  pr.LowerDistanceParameters(u, v);
  out.push_back(u);
  out.push_back(v);
  return out;
  EX_QCATCH("surface parameter", rust::Vec<double>())
}

/// Whether (u, v) lies inside the face or on its boundary.
inline bool ex_face_has_uv(const TopoDS_Shape &f, double u, double v, double tol) {
  try {
    BRepTopAdaptor_FClass2d cl(TopoDS::Face(f), tol);
    TopAbs_State st = cl.Perform(gp_Pnt2d(u, v));
    return st == TopAbs_IN || st == TopAbs_ON;
  } catch (...) {
    return false;
  }
}

/// Triangulate (linear deflection in mm, angular in radians; `relative` scales the linear one by edge size)
/// and return [vertex count, triangle count, xyz…, i j k…, face number of each triangle…]; triangles face
/// outward.
inline rust::Vec<double> ex_triangulate(const TopoDS_Shape &s, double linear, double angular, bool relative) {
  rust::Vec<double> out;
  EX_QTRY
  BRepMesh_IncrementalMesh mesh(s, linear, relative, angular, Standard_True);
  std::vector<double> verts;
  std::vector<double> tris;
  std::vector<double> faces;
  TopTools_IndexedMapOfShape fm;
  TopExp::MapShapes(s, TopAbs_FACE, fm);
  for (int fi = 1; fi <= fm.Extent(); fi++) {
    const TopoDS_Face &face = TopoDS::Face(fm(fi));
    TopLoc_Location loc;
    Handle(Poly_Triangulation) tri = BRep_Tool::Triangulation(face, loc);
    if (tri.IsNull())
      continue;
    size_t base = verts.size() / 3;
    gp_Trsf t = loc.Transformation();
    for (int i = 1; i <= tri->NbNodes(); i++) {
      gp_Pnt p = tri->Node(i).Transformed(t);
      verts.push_back(p.X()); verts.push_back(p.Y()); verts.push_back(p.Z());
    }
    bool rev = face.Orientation() == TopAbs_REVERSED;
    for (int i = 1; i <= tri->NbTriangles(); i++) {
      int a, b, c;
      tri->Triangle(i).Get(a, b, c);
      if (rev)
        std::swap(b, c);
      tris.push_back(base + a - 1); tris.push_back(base + b - 1); tris.push_back(base + c - 1);
      faces.push_back(fi - 1);
    }
  }
  out.push_back(verts.size() / 3);
  out.push_back(tris.size() / 3);
  for (double v : verts) out.push_back(v);
  for (double v : tris) out.push_back(v);
  for (double v : faces) out.push_back(v);
  return out;
  EX_QCATCH("triangulate", rust::Vec<double>())
}

/// Planar loft between wires without reordering them (FreeCAD's makeLoft: compatibility not checked).
inline std::unique_ptr<TopoDS_Shape> ex_loft2(const TopTools_ListOfShape &wires, bool solid, bool ruled, bool check) {
  EX_TRY
  BRepOffsetAPI_ThruSections mk(solid, ruled);
  for (auto it = wires.begin(); it != wires.end(); ++it) {
    if (it->ShapeType() == TopAbs_VERTEX)
      mk.AddVertex(TopoDS::Vertex(*it));
    else if (it->ShapeType() == TopAbs_EDGE)
      mk.AddWire(BRepBuilderAPI_MakeWire(TopoDS::Edge(*it)).Wire());
    else
      mk.AddWire(TopoDS::Wire(*it));
  }
  mk.CheckCompatibility(check);
  mk.Build();
  if (!mk.IsDone())
    return ex_null("loft failed");
  return ex_wrap(mk.Shape());
  EX_CATCH("loft")
}

/// Boolean with options: op 0 fuse, 1 cut, 2 common; arguments and tools as lists; non-destructive (the
/// inputs are never modified); `parallel` runs OCCT's boolean on several threads.
inline std::unique_ptr<TopoDS_Shape> ex_boolean_opts(const TopTools_ListOfShape &args, const TopTools_ListOfShape &tools, int op,
                                                     double fuzzy, bool parallel) {
  EX_TRY
  std::unique_ptr<BRepAlgoAPI_BooleanOperation> bop;
  if (op == 0)
    bop.reset(new BRepAlgoAPI_Fuse());
  else if (op == 1)
    bop.reset(new BRepAlgoAPI_Cut());
  else
    bop.reset(new BRepAlgoAPI_Common());
  bop->SetArguments(args);
  bop->SetTools(tools);
  if (fuzzy > 0.0)
    bop->SetFuzzyValue(fuzzy);
  bop->SetNonDestructive(Standard_True);
  bop->SetRunParallel(parallel);
  bop->Build();
  if (!bop->IsDone() || bop->HasErrors())
    return ex_null(op == 0 ? "fuse failed" : op == 1 ? "cut failed" : "common failed");
  return ex_wrap(bop->Shape());
  EX_CATCH("boolean")
}

/// Chamfer of equal distances on edges, each on its first neighbouring face (FreeCAD's makeChamfer).
inline std::unique_ptr<TopoDS_Shape> ex_chamfer_faces(const TopoDS_Shape &s, const TopTools_ListOfShape &edges, double d) {
  EX_TRY
  BRepFilletAPI_MakeChamfer mk(s);
  TopTools_IndexedDataMapOfShapeListOfShape map;
  TopExp::MapShapesAndAncestors(s, TopAbs_EDGE, TopAbs_FACE, map);
  for (auto it = edges.begin(); it != edges.end(); ++it) {
    const TopoDS_Edge &e = TopoDS::Edge(*it);
    if (!map.Contains(e) || map.FindFromKey(e).IsEmpty())
      continue;
    mk.Add(d, d, e, TopoDS::Face(map.FindFromKey(e).First()));
  }
  mk.Build();
  if (!mk.IsDone())
    return ex_null("the chamfer could not be built (distance too large?)");
  return ex_wrap(mk.Shape());
  EX_CATCH("chamfer")
}

/// Write a shape's triangulation (made with these deflections) as a binary STL.
inline bool ex_write_stl(const TopoDS_Shape &s, rust::Str path, double linear, double angular) {
  try {
    BRepMesh_IncrementalMesh mesh(s, linear, Standard_False, angular, Standard_True);
    StlAPI_Writer w;
    w.ASCIIMode() = Standard_False;
    return w.Write(s, std::string(path).c_str());
  } catch (const Standard_Failure &e) {
    ex_err_slot() = std::string("STL: ") + e.GetMessageString();
    return false;
  } catch (...) {
    ex_err_slot() = "STL export failed";
    return false;
  }
}

/// A null shape (FreeCAD's Part.Shape()).
inline std::unique_ptr<TopoDS_Shape> ex_null_shape() { return std::unique_ptr<TopoDS_Shape>(new TopoDS_Shape()); }

#include <BRepTools_WireExplorer.hxx>

/// The outer wire of a face.
inline std::unique_ptr<TopoDS_Shape> ex_outer_wire(const TopoDS_Shape &f) {
  EX_TRY
  TopoDS_Wire w = BRepTools::OuterWire(TopoDS::Face(f));
  if (w.IsNull())
    return ex_null("the face has no outer wire");
  return ex_wrap(w);
  EX_CATCH("outer wire")
}

/// A wire's edges (kind 4) or vertices (kind 5) in the order they connect.
inline std::unique_ptr<TopTools_IndexedMapOfShape> ex_ordered(const TopoDS_Shape &w, int kind) {
  std::unique_ptr<TopTools_IndexedMapOfShape> m(new TopTools_IndexedMapOfShape());
  try {
    TopoDS_Vertex last;
    for (BRepTools_WireExplorer ex(TopoDS::Wire(w)); ex.More(); ex.Next()) {
      if (kind == 4)
        m->Add(ex.Current());
      else
        m->Add(ex.CurrentVertex());
      TopoDS_Vertex v1, v2;
      TopExp::Vertices(ex.Current(), v1, v2, Standard_True);
      last = v2;
    }
    if (kind == 5 && !last.IsNull())
      m->Add(last);
  } catch (...) {
  }
  return m;
}

/// The shape's edges as polylines for drawing, no further than `deflection` from the curves: [edge count,
/// then for each edge its point count and xyz…]. Seams (an edge closed on its only face) and degenerate
/// edges are left out.
inline rust::Vec<double> ex_edge_polylines(const TopoDS_Shape &s, double deflection) {
  rust::Vec<double> out;
  EX_QTRY
  TopTools_IndexedDataMapOfShapeListOfShape map;
  TopExp::MapShapesAndAncestors(s, TopAbs_EDGE, TopAbs_FACE, map);
  std::vector<std::vector<gp_Pnt>> lines;
  for (int i = 1; i <= map.Extent(); i++) {
    const TopoDS_Edge &e = TopoDS::Edge(map.FindKey(i));
    if (BRep_Tool::Degenerated(e))
      continue;
    const TopTools_ListOfShape &faces = map.FindFromIndex(i);
    bool seam = false;
    for (auto it = faces.begin(); it != faces.end(); ++it)
      if (BRep_Tool::IsClosed(e, TopoDS::Face(*it)))
        seam = true;
    if (seam)
      continue;
    try {
      BRepAdaptor_Curve c(e);
      GCPnts_QuasiUniformDeflection d(c, std::max(deflection, 1e-4));
      if (!d.IsDone())
        continue;
      std::vector<gp_Pnt> pts;
      for (int k = 1; k <= d.NbPoints(); k++)
        pts.push_back(d.Value(k));
      lines.push_back(pts);
    } catch (...) {
    }
  }
  out.push_back((double)lines.size());
  for (auto &l : lines) {
    out.push_back((double)l.size());
    for (auto &p : l) {
      out.push_back(p.X()); out.push_back(p.Y()); out.push_back(p.Z());
    }
  }
  return out;
  EX_QCATCH("edge polylines", rust::Vec<double>())
}

#include <STEPCAFControl_Writer.hxx>
#include <STEPControl_StepModelType.hxx>
#include <TDocStd_Document.hxx>
#include <TDataStd_Name.hxx>
#include <TDF_Label.hxx>
#include <XCAFApp_Application.hxx>
#include <XCAFDoc_ColorTool.hxx>
#include <XCAFDoc_DocumentTool.hxx>
#include <XCAFDoc_ShapeTool.hxx>
#include <Quantity_Color.hxx>
#include <Interface_Static.hxx>
#include <Message.hxx>
#include <Message_Messenger.hxx>
#include <Message_PrinterOStream.hxx>

/// Send OpenCascade's own messages (such as the STEP writer's transfer statistics) to stderr instead of
/// stdout, warnings and worse only, for command-line modes whose stdout is their result.
inline void ex_messages_to_stderr() {
  const Handle(Message_Messenger) &m = Message::DefaultMessenger();
  m->RemovePrinters(STANDARD_TYPE(Message_PrinterOStream));
  m->AddPrinter(new Message_PrinterOStream("cerr", Standard_False, Message_Warning));
}

/// Write an assembly to STEP with names, colours and structure. Nodes in order: `shapes` (a null shape for
/// an assembly), `parents` (index of the parent node, -1 at the top; parents come before children),
/// `names`, `colors` (r g b per node, 0–1; negative: none). Returns false with ex_last_error() on failure.
inline bool ex_write_step_assembly(const TopTools_ListOfShape &shapes, rust::Slice<const int32_t> parents,
                                   rust::Slice<const rust::String> names, rust::Slice<const double> colors, rust::Str path) {
  try {
    Handle(TDocStd_Document) doc = new TDocStd_Document("XmlXCAF");
    TDF_Label main = doc->Main();
    Handle(XCAFDoc_ShapeTool) st = XCAFDoc_DocumentTool::ShapeTool(main);
    Handle(XCAFDoc_ColorTool) ct = XCAFDoc_DocumentTool::ColorTool(main);
    size_t n = parents.size();
    std::vector<TopoDS_Shape> list;
    for (auto it = shapes.begin(); it != shapes.end(); ++it)
      list.push_back(*it);
    if (list.size() != n || names.size() != n || colors.size() != 3 * n) {
      ex_err_slot() = "STEP: shapes, parents, names and colours differ in length";
      return false;
    }
    std::vector<TDF_Label> labels(n);
    std::vector<bool> is_asm(n, false);
    for (size_t i = 0; i < n; i++)
      if (list[i].IsNull())
        is_asm[i] = true;
    for (size_t i = 0; i < n; i++) {
      TDF_Label l = is_asm[i] ? st->NewShape() : st->AddShape(list[i], Standard_False);
      TDataStd_Name::Set(l, TCollection_ExtendedString(std::string(names[i]).c_str(), Standard_True));
      if (colors[3 * i] >= 0)
        ct->SetColor(l, Quantity_Color(colors[3 * i], colors[3 * i + 1], colors[3 * i + 2], Quantity_TOC_RGB), XCAFDoc_ColorGen);
      labels[i] = l;
    }
    for (size_t i = 0; i < n; i++) {
      int p = parents[i];
      if (p < 0)
        continue;
      if (p >= (int)n || !is_asm[p]) {
        ex_err_slot() = "STEP: a parent must be an assembly node";
        return false;
      }
      TDF_Label c = st->AddComponent(labels[p], labels[i], TopLoc_Location());
      TDataStd_Name::Set(c, TCollection_ExtendedString(std::string(names[i]).c_str(), Standard_True));
    }
    st->UpdateAssemblies();
    STEPCAFControl_Writer w;
    w.SetColorMode(Standard_True);
    w.SetNameMode(Standard_True);
    Interface_Static::SetCVal("write.step.unit", "MM");
    if (!w.Transfer(doc, STEPControl_AsIs)) {
      ex_err_slot() = "STEP: transfer failed";
      return false;
    }
    if (w.Write(std::string(path).c_str()) != IFSelect_RetDone) {
      ex_err_slot() = "STEP: could not write the file";
      return false;
    }
    return true;
  } catch (const Standard_Failure &e) {
    ex_err_slot() = std::string("STEP: ") + e.GetMessageString();
    return false;
  } catch (...) {
    ex_err_slot() = "STEP export failed";
    return false;
  }
}

/// Volume shared by each pair (a[i], b[i]), the pairs spread over `threads` threads (0: all cores). For
/// interference checks over many parts; the inputs are never modified.
inline rust::Vec<double> ex_common_volumes(const TopTools_ListOfShape &a, const TopTools_ListOfShape &b, int threads) {
  rust::Vec<double> out;
  std::vector<TopoDS_Shape> va(a.begin(), a.end()), vb(b.begin(), b.end());
  size_t n = std::min(va.size(), vb.size());
  std::vector<double> res(n, -1.0);
  std::atomic<size_t> next(0);
  auto work = [&]() {
    for (size_t i = next++; i < n; i = next++) {
      try {
        BRepAlgoAPI_Common op;
        TopTools_ListOfShape la, lb;
        la.Append(va[i]);
        lb.Append(vb[i]);
        op.SetArguments(la);
        op.SetTools(lb);
        op.SetNonDestructive(Standard_True);
        op.SetRunParallel(Standard_False);
        op.Build();
        if (!op.IsDone() || op.HasErrors())
          continue;
        GProp_GProps g;
        BRepGProp::VolumeProperties(op.Shape(), g);
        res[i] = g.Mass();
      } catch (...) {
      }
    }
  };
  unsigned hw = std::max(1u, std::thread::hardware_concurrency());
  unsigned nt = threads > 0 ? (unsigned)threads : hw;
  nt = (unsigned)std::max<size_t>(1, std::min<size_t>(nt, n));
  std::vector<std::thread> th;
  for (unsigned t = 0; t < nt; t++)
    th.emplace_back(work);
  for (auto &t : th)
    t.join();
  for (double v : res)
    out.push_back(v);
  return out;
}
