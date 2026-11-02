# 3D-printed plastics in the Simulation tab

<img alt="A PLA bracket printed at 20 % gyroid with three walls: the print settings under the material, von Mises stress, and the walls' and infill's factors of safety" src="screenshots/fea-printed.png" width="100%">

Turn on **3D printed** under the material when the parts are a printable plastic. A printed part is
then a solid shell (its walls and its top and bottom layers) around a sparse infill, and it is weaker
between layers than along them.

## Settings

| Setting | Default | Meaning |
| --- | --- | --- |
| Print model | Layered | Layered: transversely isotropic about the build direction. Isotropic: one averaged modulus and the weakest (Z) strength everywhere. |
| Infill | 20 %, grid | Sparse density and pattern: grid / rectilinear, triangles, honeycomb, gyroid / cubic. |
| Walls | 2 × 0.45 mm | Wall loops × line width: the side shell. |
| Top / bottom | 4 × 0.2 mm | Solid layers × layer height: the shell on faces within 45° of facing up or down. |
| Build direction | +Z | The printer's Z in the Part Studio's axes. |
| Layer bond | × 1 | Layer bond over the data sheet's Z strength: 0.5–0.8 for tall or fast prints, full part cooling or a cold enclosure. |

The agent and scripts set the same with `print: {model, infill, pattern, walls, line_width, top_bottom,
layer_height, build_dir, layer_bond}` on `create_simulation` / `edit_simulation` (`true` for the defaults, `null` for
solid).

## How it is modelled

- **Shell and infill.** Every node's distance to the part's surface is divided by the shell thickness
  there (top/bottom skin where the nearest surface faces up or down, walls elsewhere). Each element's
  shell share is the fraction of 35 sample points inside it that lie in the shell, and its material is
  that blend of shell and infill (in 1/8 steps). Mesh at about the wall thickness on faces where the
  stress matters, or the walls are blended into the infill (a warning says so).
- **Solid printed material** (the shell): makers' data for 100 % infill specimens printed flat (X-Y) and
  upright (Z), ISO 527.

  | Material | E X-Y / Z (MPa) | Strength X-Y / Z (MPa) | Source |
  | --- | --- | --- | --- |
  | PLA | 3427 / 3065 | 52.3 / 40.5 | Polymaker PolyLite PLA |
  | PETG | 2117 / 1899 | 50.8 / 42.8 | Polymaker PolyLite PETG |
  | ABS | 2247 / 2081 | 33.4 / 29.7 | Polymaker PolyLite ABS |
  | ASA | 2379 / 1965 | 43.8 / 32.0 | Polymaker ASA |
  | PC | 2497 / 2371 | 69.1 / 52.8 | Polymaker PolyLite PC |
  | Nylon (dry) | 2703 / 2209 | 78.0 / 45.8 | Polymaker PolyMide CoPA, dry |
  | Nylon (humid) | 724 / 660 | 34.3 / 13.7 | Polymaker PolyMide CoPA, wet (6 % water) |
  | PLA-CF | 3281 / 2213 | 31.2 / 15.1 | Polymaker PolyLite PLA-CF |
  | PETG-CF | 2460 / 1340 | 35 / 29 | Bambu Lab PETG-CF |
  | ASA-CF | 4200 / 2290 | 34 / 30 | Bambu Lab ASA-CF |
  | PET-CF | 4730 / 2160 | 74 / 35 | Bambu Lab PET-CF |
  | PPS-CF | 8230 / 2850 | 87 / 24 | Bambu Lab PPS-CF (not annealed) |
  | PA6-CF | 8637 / 3760 | 109.3 / 54.0 | Polymaker Fiberon PA6-CF20, dry, annealed |
  | PA6-CF (humid) | 2508 / 1056 | 54.7 / 25.5 | Polymaker Fiberon PA6-CF20, wet (5 % water) |
  | PA12-CF | 3311 / 1807 | 77.4 / 52.2 | Polymaker Fiberon PA12-CF10, dry, annealed |
  | PA12-CF (humid) | 3132 / 1622 | 71.7 / 42.1 | Polymaker Fiberon PA12-CF10, wet (3 % water) |

- **Infill** scales the solid's properties with its density ρ (Gibson–Ashby): exponent 1 where the
  infill's walls stretch, 2–3 where they bend.

  | Pattern | Layer plane stiffness | Z stiffness | Strength (plane / Z) |
  | --- | --- | --- | --- |
  | Grid / rectilinear | ρ/4 | ρ | ρ/4 / ρ |
  | Triangles | ρ/3 | ρ | ρ/3 / ρ |
  | Honeycomb | 1.5 ρ³ | ρ | 0.5 ρ² / ρ |
  | Gyroid / cubic | 0.8 ρ² | 0.8 ρ² | 0.3 ρ^1.5 |

  Each law is lifted by (1 − law(1)) ρ³ so that 100 % infill is the solid. Infill density also scales
  the mass.
- **Layered stiffness:** in-plane E_xy and ν, through-layer E_z, ν_zp = ν √(E_z/E_xy), interlayer shear
  modulus E_z / 2(1 + ν).
- **Failure (factor of safety):** the larger of two checks.
  - *Bulk:* Raghava's pressure-modified von Mises, vm² + (C − T) I₁ = C T, with T the in-plane strength
    and C = 1.3 T. Polymers yield later under compression than in tension (FDM parts 1.2–2× stronger in
    compression, Ahn et al. 2002), and this gives exactly T in uniaxial tension, C in compression and
    √(CT/3) in shear.
  - *Layer bond:* √((⟨σn⟩/Z)² + (τ/(S + μ⟨−σn⟩))²) on the layer planes, with σn the stress across the
    layers, τ the shear along them, Z the Z strength × layer bond, S = Z/√3 and μ = 0.25. Tension across
    the layers counts, compression does not, but it raises the shear the bond carries (friction, as in
    Puck's criterion for laminates).

  Nodes in the shell use the shell's strengths, nodes inside the infill's. The isotropic model uses the
  bulk check against the Z strength × layer bond.

  Compared with alternatives: plain von Mises is unsafe for tension across the layers and too cautious in
  compression; Tsai–Wu and Hashin need compressive and shear strengths the data sheets don't give, so
  their extra terms would be guesses. The data-sheet Z value is the biggest uncertainty, and the layer
  bond factor is where to put it.

`examples/fea-printed.json` solves the same PLA bracket printed flat and printed on its side: on its
side the bending runs along the layers, so it is about 20 % stiffer and its walls' factor of safety rises
from 3.0 to 3.5.

## Limits

- Linear elastic: no yielding, creep, fatigue or fracture. The data-sheet values are best-case prints:
  real parts' layer bonds are often 50–80 % of them (set the layer bond), and humid nylon loses most of
  its stiffness. Long-term loads creep: plan on 30–50 % of the short-term strength for months of load.
- The layer plane is isotropic (±45° alternating rasters); single-direction rasters are stiffer along
  the road than across it.
- Advanced mode treats printed parts as solid and isotropic.

## References

- Polymaker technical data sheets: PolyLite PLA, PETG, ABS, PC, PLA-CF; Polymaker ASA; PolyMide CoPA.
  https://wiki.polymaker.com/polymaker-products/more-about-our-products/documents/technical-data-sheets
- Bambu Lab technical data sheets: PETG-CF (V3), ASA-CF (V1), PET-CF (V3), PPS-CF (V1).
- Polymaker Fiberon technical data sheets: PA6-CF20 (V1.1), PA12-CF10 (V1.1).
  https://fiberon.polymaker.com/wp-content/uploads/TDS_FIBERON-PA6-CF20_V1.1_EN.pdf,
  https://fiberon.polymaker.com/wp-content/uploads/TDS_FIBERON-PA12-CF10_V1.1_EN.pdf
- S.-H. Ahn, M. Montero, D. Odell, S. Roundy, P. K. Wright (2002), Anisotropic material properties of
  fused deposition modeling ABS, Rapid Prototyping Journal 8(4) 248–257, doi:10.1108/13552540210441166.
- C. Casavola, A. Cazzato, V. Moramarco, C. Pappalettere (2016), Orthotropic mechanical properties of
  fused deposition modelling parts described by classical laminate theory, Materials & Design 90
  453–458, doi:10.1016/j.matdes.2015.11.009.
- T. J. Gordelier, P. R. Thies, L. Turner, L. Johanning (2019), Optimising the FDM additive manufacturing
  process to achieve maximum tensile strength: a state-of-the-art review, Rapid Prototyping Journal.
- S. N. Cerda-Avila, H. I. Medellín-Castillo, T. Lim (2021), Analytical models to estimate the structural
  behaviour of fused deposition modelling components, Rapid Prototyping Journal 27(4) 658–670,
  doi:10.1108/RPJ-07-2020-0145.
- H. Rostro-González et al. (2024), Additive manufacturing gyroid structures used as crash energy
  management, Computation 12(12) 248, doi:10.3390/computation12120248.
- D. Hodzic, A. Pandzic (2021), Influence of infill design on compressive and flexural mechanical
  properties of FDM printed PLA material, Proc. 32nd DAAAM Int. Symposium 191–199,
  doi:10.2507/32nd.daaam.proceedings.029.
- J. J. Fekiač et al. (2025), Influence of infill geometry and density on the mechanical properties of
  3D-printed polylactic acid structure, J. Manuf. Mater. Process. 9(4) 134, doi:10.3390/jmmp9040134.
- R. Raghava, R. M. Caddell, G. S. Y. Yeh (1973), The macroscopic
  yield behaviour of polymers, J. Mater. Sci. 8 225–232, doi:10.1007/BF00550671.
- A. Puck, H. Schürmann (1998), Failure analysis of FRP laminates by means of physically based
  phenomenological models, Compos. Sci. Technol. 58 1045–1067, doi:10.1016/S0266-3538(96)00140-6.
- L. J. Gibson, M. F. Ashby (1997), Cellular Solids: Structure and Properties, 2nd ed., Cambridge
  University Press.
