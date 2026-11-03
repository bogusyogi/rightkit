# Adversarial review round 1 — candidate p07 (480×600 draft)

Reviewer: Oracle subagent, read-only, given the candidate plus brand references 02/04 & two Knife Informer photos of the titanium Fenrir. Verdict **NO**.

Biggest reason: knife reads as an untextured grey CAD model on three flat bands (black void / clean box / smooth tan floor); no environment-reflection gradient or tumble marks on Ti or blade; zero wear, grain, dust or depth.

Ranked defects & disposition:

1. Ti reads as sandblasted plastic; needs warm base, scratch glints, large key for facet falloff → **done p08**: warmer brighter Ti, scratch density .05, 0.48×0.96 m graded key card, 2 cm accent.
2. Blade flat grey plate, no plunge/grind read, card only hits tip → **p08–p11**: blade card re-aimed via `blade_reflection` for the posed normal, strength tuned 1.0→2.4→1.3→1.6; accent position found to blow the flat when near the camera axis (p09/p10), moved back up (p11).
3. Knife floats, no contact shadow on box → **p08**: gap 0.5 mm, back-lean 12°, contact computed on the yawed face.
4. Three horizontal bands, tip tangent to frame → **p08**: box yawed (−22°), knife shifted off-centre (`shift_x`/`shift_y`), occupancy .86.
5. Box brand-new & textureless → **p08**: `worn_paint` (bare metal on edges & chips, dust roughness, dents); **p11**: specular cut so the near-black face stops reading grey.
6. Floor foam-board, too bright → **p08**: value .045, red-brown tint, 18 cm tile, normal .7, coat sheen .12.
7. No visible DoF/foreground → **p08**: f/2.8, foreground bokeh element (first too large, p08; shrunk & moved, p09+).
8. Two-tone warm/black, no separation → **p08**: cool rim 2 W knife-linked, fill .3–.4, textured backdrop at 6–8 %.
9. Pivot/hardware read as decals; heel plate → not modelled (CAD has flush pivot); heel plate is the steel liner, kept.
10. Jimping reads as serrations; flipper must silhouette → **p08**: lean 54°, roll −8°.
Not done: brand etch & hex logo (no asset; not fabricated).
