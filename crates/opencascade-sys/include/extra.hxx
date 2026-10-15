// Extra OpenCascade operations for cad-rust-app. Every function catches OCCT
// exceptions and returns a null shape instead (cxx can't translate
// Standard_Failure, and an uncaught one aborts the process). The message of
// the last failure is available from ex_last_error().
#pragma once
#include "rust/cxx.h"
#include <memory>
#include <string>

#include <BRepAlgoAPI_Common.hxx>
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
#include <ShapeUpgrade_UnifySameDomain.hxx>
#include <Standard_Failure.hxx>
#include <TColgp_HArray1OfPnt.hxx>
#include <TopExp_Explorer.hxx>
#include <TopTools_ListOfShape.hxx>
#include <TopoDS.hxx>
#include <TopoDS_Compound.hxx>
#include <BRep_Builder.hxx>
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
    ShapeUpgrade_UnifySameDomain u(s, Standard_True, Standard_True, Standard_False);
    u.Build();
    TopoDS_Shape r = u.Shape();
    return r.IsNull() ? s : r;
  } catch (...) {
    return s;
  }
}

inline std::unique_ptr<TopoDS_Shape> ex_unify(const TopoDS_Shape &s) { return ex_wrap(ex_unify_raw(s)); }

inline std::unique_ptr<TopoDS_Shape> ex_boolean(const TopoDS_Shape &a, const TopoDS_Shape &b, int op) {
  EX_TRY
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
  bop->SetFuzzyValue(1e-6);
  bop->SetRunParallel(true);
  bop->Build();
  if (!bop->IsDone() || bop->HasErrors())
    return ex_null("boolean operation failed");
  return ex_wrap(ex_unify_raw(bop->Shape()));
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

inline std::unique_ptr<TopoDS_Shape> ex_sweep(const TopoDS_Shape &profile_wire, const TopoDS_Shape &spine, bool frenet) {
  EX_TRY
  BRepOffsetAPI_MakePipeShell mk(TopoDS::Wire(spine));
  mk.SetMode(frenet);
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
