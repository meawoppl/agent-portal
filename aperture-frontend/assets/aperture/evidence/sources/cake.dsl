module aperture_cake

# Original presentation geometry, generated for Agent Portal/Aperture demo v4.
# THE CAKE IS A LIE. THE MESH IS WATERTIGHT.
# Decorative model only; not a food, recipe, or manufacturing specification.

command wedge(radius: float, height: float, z: float) -> solid:
    let upper: solid = rotate(translate(box(600.0, 600.0, height + 2.0), 0.0, -300.0, height / 2.0), 0.0, 0.0, 33.6900675)
    let lower: solid = rotate(translate(box(600.0, 600.0, height + 2.0), 0.0, 300.0, height / 2.0), 0.0, 0.0, -33.6900675)
    emit translate(intersection(intersection(cylinder(radius, height), upper), lower), 0.0, 0.0, z)

command CAKE_IS_A_LIE(
    filling: float @ui(widget="slider", min=0.5, max=10.0, step=0.5, label="Berry filling thickness") = 6.0,
    frosting: float @ui(widget="slider", min=2.0, max=10.0, step=0.5, label="Top frosting thickness") = 7.0,
    explode: float @ui(widget="slider", min=0.0, max=14.0, step=1.0, label="Explode layer spacing") = 1.5
) -> solid:
    require filling > 0.0
    require frosting > 0.0
    require explode >= 0.0
    let plate: solid = translate(cylinder(90.0, 5.0), 65.0, 0.0, -8.0)
    let rim: solid = translate(difference(cylinder(90.0, 2.0), translate(cylinder(85.0, 4.0), 0.0, 0.0, -1.0)), 65.0, 0.0, -3.0)
    let serving: solid = union(plate, rim)
    let base: solid = wedge(127.0, 3.0, 0.0)
    let sponge1: solid = wedge(125.0, 19.0, 3.0 + explode)
    let jam1: solid = wedge(125.8, filling, 22.0 + 2.0 * explode)
    let cream1: solid = wedge(126.0, 3.0, 22.0 + filling + 3.0 * explode)
    let sponge2: solid = wedge(125.0, 19.0, 25.0 + filling + 4.0 * explode)
    let jam2: solid = wedge(125.8, filling, 44.0 + filling + 5.0 * explode)
    let cream2: solid = wedge(126.0, 3.0, 44.0 + 2.0 * filling + 6.0 * explode)
    let sponge3: solid = wedge(125.0, 19.0, 47.0 + 2.0 * filling + 7.0 * explode)
    let top: float = 66.0 + 2.0 * filling + 8.0 * explode
    let icing: solid = wedge(128.0, frosting, top)
    let garnish: float = top + frosting
    let cherry1: solid = translate(sphere(8.0), 80.0, -14.0, garnish + 7.0)
    let cherry2: solid = translate(sphere(8.0), 91.0, 2.0, garnish + 7.0)
    let stem1: solid = translate(rotate(cylinder(0.9, 17.0), 0.0, -22.0, 0.0), 80.0, -14.0, garnish + 13.0)
    let stem2: solid = translate(rotate(cylinder(0.9, 15.0), 18.0, 25.0, 0.0), 91.0, 2.0, garnish + 13.0)
    let candle: solid = translate(cylinder(3.0, 28.0), 54.0, 0.0, garnish)
    let wick: solid = translate(cylinder(0.7, 4.0), 54.0, 0.0, garnish + 28.0)
    let flame: solid = translate(union(sphere(3.4), cone(3.4, 0.25, 10.0)), 54.0, 0.0, garnish + 34.0)
    let dollop0: solid = translate(union(sphere(4.5), cone(4.5, 0.5, 8.0)), 104.24776, -53.11689, garnish + 3.5)
    let dollop1: solid = translate(union(sphere(4.5), cone(4.5, 0.5, 8.0)), 111.27361, -36.15499, garnish + 3.5)
    let dollop2: solid = translate(union(sphere(4.5), cone(4.5, 0.5, 8.0)), 115.55954, -18.30283, garnish + 3.5)
    let dollop3: solid = translate(union(sphere(4.5), cone(4.5, 0.5, 8.0)), 117.00000, 0.00000, garnish + 3.5)
    let dollop4: solid = translate(union(sphere(4.5), cone(4.5, 0.5, 8.0)), 115.55954, 18.30283, garnish + 3.5)
    let dollop5: solid = translate(union(sphere(4.5), cone(4.5, 0.5, 8.0)), 111.27361, 36.15499, garnish + 3.5)
    let dollop6: solid = translate(union(sphere(4.5), cone(4.5, 0.5, 8.0)), 104.24776, 53.11689, garnish + 3.5)
    let sprinkle0: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 32.867), 84.987, 25.991, garnish + 0.65)
    let sprinkle1: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 120.760), 107.863, -29.696, garnish + 0.65)
    let sprinkle2: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 27.234), 35.343, 8.198, garnish + 0.65)
    let sprinkle3: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 137.870), 84.525, 17.074, garnish + 0.65)
    let sprinkle4: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 177.824), 63.310, 20.841, garnish + 0.65)
    let sprinkle5: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 172.469), 37.248, 0.566, garnish + 0.65)
    let sprinkle6: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 54.828), 87.132, 30.000, garnish + 0.65)
    let sprinkle7: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 136.900), 104.242, -38.923, garnish + 0.65)
    let sprinkle8: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 58.837), 95.460, -29.408, garnish + 0.65)
    let sprinkle9: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 66.060), 52.429, 1.986, garnish + 0.65)
    let sprinkle10: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 56.910), 99.495, 38.024, garnish + 0.65)
    let sprinkle11: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 17.443), 99.582, -38.573, garnish + 0.65)
    let sprinkle12: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 147.447), 81.210, 21.859, garnish + 0.65)
    let sprinkle13: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 47.954), 107.358, 34.758, garnish + 0.65)
    let sprinkle14: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 16.847), 87.423, 33.412, garnish + 0.65)
    let sprinkle15: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 25.758), 67.554, -21.019, garnish + 0.65)
    let sprinkle16: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 175.276), 66.364, 10.176, garnish + 0.65)
    let sprinkle17: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 179.526), 38.508, 0.189, garnish + 0.65)
    let sprinkle18: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 43.669), 89.201, 30.000, garnish + 0.65)
    let sprinkle19: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 115.906), 78.477, 30.000, garnish + 0.65)
    let sprinkle20: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 143.059), 79.895, 30.000, garnish + 0.65)
    let sprinkle21: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 149.693), 60.283, -1.688, garnish + 0.65)
    let sprinkle22: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 103.384), 48.042, -13.598, garnish + 0.65)
    let sprinkle23: solid = translate(rotate(box(1.8, 5.0, 1.3), 0.0, 0.0, 44.809), 62.766, 21.136, garnish + 0.65)
    emit compound(serving, base, sponge1, jam1, cream1, sponge2, jam2, cream2, sponge3, icing, cherry1, cherry2, stem1, stem2, candle, wick, flame, dollop0, dollop1, dollop2, dollop3, dollop4, dollop5, dollop6, sprinkle0, sprinkle1, sprinkle2, sprinkle3, sprinkle4, sprinkle5, sprinkle6, sprinkle7, sprinkle8, sprinkle9, sprinkle10, sprinkle11, sprinkle12, sprinkle13, sprinkle14, sprinkle15, sprinkle16, sprinkle17, sprinkle18, sprinkle19, sprinkle20, sprinkle21, sprinkle22, sprinkle23)

# SINGLE-PIECE PRINT EDITION: fused, flat underside, no slender stems,
# candle, wick, detached flame, or small sprinkles. Units are millimetres.
command THERMOPLASTIC_CAKE(
    print_scale: float @ui(widget="slider", min=0.4, max=1.0, step=0.1, label="Print scale") = 0.6
) -> solid:
    require print_scale > 0.0
    let plate: solid = translate(cylinder(90.0, 4.0), 65.0, 0.0, 0.0)
    let body: solid = wedge(125.0, 75.0, 3.0)
    let icing: solid = wedge(128.0, 8.0, 77.0)
    let cherry1: solid = translate(sphere(8.0), 80.0, -14.0, 92.0)
    let cherry2: solid = translate(sphere(8.0), 91.0, 2.0, 92.0)
    let cream0: solid = translate(union(sphere(4.5), cone(4.5, 0.5, 8.0)), 104.24776, -53.11689, 88.5)
    let cream1: solid = translate(union(sphere(4.5), cone(4.5, 0.5, 8.0)), 111.27361, -36.15499, 88.5)
    let cream2: solid = translate(union(sphere(4.5), cone(4.5, 0.5, 8.0)), 115.55954, -18.30283, 88.5)
    let cream3: solid = translate(union(sphere(4.5), cone(4.5, 0.5, 8.0)), 117.00000, 0.00000, 88.5)
    let cream4: solid = translate(union(sphere(4.5), cone(4.5, 0.5, 8.0)), 115.55954, 18.30283, 88.5)
    let cream5: solid = translate(union(sphere(4.5), cone(4.5, 0.5, 8.0)), 111.27361, 36.15499, 88.5)
    let cream6: solid = translate(union(sphere(4.5), cone(4.5, 0.5, 8.0)), 104.24776, 53.11689, 88.5)
    emit scale(union(plate, body, icing, cherry1, cherry2, cream0, cream1, cream2, cream3, cream4, cream5, cream6), print_scale, print_scale, print_scale)
