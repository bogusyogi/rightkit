"""Generic procedural stonewash metal study materials (unapproved drafts)."""

import bpy


VARIANTS = (1, 2, 3)
SCRATCH_DEPTH_UM = 60
ROUGHNESS = 0.42
METALLIC = 1.0
BEVEL_RADIUS_M = 0.0002


def _node(nodes, kind, name, location):
    node = nodes.new(kind)
    node.label = name
    node.location = location
    return node


def _link(links, source, output, target, input_name):
    links.new(source.outputs[output], target.inputs[input_name])


def _math(nodes, links, operation, a, b, location):
    node = _node(nodes, "ShaderNodeMath", operation, location)
    node.operation = operation
    node.inputs[0].default_value = a
    node.inputs[1].default_value = b
    return node


def _scratch_noise(nodes, links, world_position, angle, index):
    rotated = _node(nodes, "ShaderNodeVectorRotate", "Scratch direction {}".format(index), (-1000, -700 - index * 220))
    rotated.rotation_type = "AXIS_ANGLE"
    rotated.inputs["Axis"].default_value = (0.0, 0.0, 1.0)
    rotated.inputs["Angle"].default_value = angle
    _link(links, world_position, "Position", rotated, "Vector")

    mm = _node(nodes, "ShaderNodeVectorMath", "World metres to millimetres", (-800, -700 - index * 220))
    mm.operation = "SCALE"
    mm.inputs[3].default_value = 1000.0
    _link(links, rotated, "Vector", mm, 0)
    stretch = _node(nodes, "ShaderNodeVectorMath", "Long irregular scratch grain", (-600, -700 - index * 220))
    stretch.operation = "MULTIPLY"
    stretch.inputs[1].default_value = (0.18, 16.0, 0.18)
    _link(links, mm, 0, stretch, 0)
    noise = _node(nodes, "ShaderNodeTexNoise", "Sparse scratch variation {}".format(index), (-400, -700 - index * 220))
    noise.inputs["Scale"].default_value = 1.0
    noise.inputs["Detail"].default_value = 2.0
    noise.inputs["Roughness"].default_value = 0.72
    _link(links, stretch, 0, noise, "Vector")
    threshold = _node(nodes, "ShaderNodeMapRange", "Irregular sparse scratch mask", (-180, -700 - index * 220))
    threshold.clamp = True
    threshold.inputs["From Min"].default_value = 0.69
    threshold.inputs["From Max"].default_value = 0.84
    threshold.inputs["To Min"].default_value = 0.0
    threshold.inputs["To Max"].default_value = 1.0
    _link(links, noise, "Fac", threshold, "Value")
    return threshold


def build_material(cell_mm):
    """Create one scale-independent study material using world-position nodes."""
    name = "DRAFT stonewash {}mm".format(cell_mm)
    material = bpy.data.materials.new(name)
    material.use_nodes = True
    material.use_fake_user = True
    material.diffuse_color = (0.42, 0.43, 0.44, 1.0)
    nodes = material.node_tree.nodes
    links = material.node_tree.links
    nodes.clear()

    output = _node(nodes, "ShaderNodeOutputMaterial", "Material output", (1250, 120))
    principled = _node(nodes, "ShaderNodeBsdfPrincipled", "Draft stonewash metal", (1000, 120))
    principled.inputs["Base Color"].default_value = (0.42, 0.43, 0.44, 1.0)
    principled.inputs["Metallic"].default_value = METALLIC
    principled.inputs["Roughness"].default_value = ROUGHNESS
    if "Anisotropic IOR Level" in principled.inputs:
        principled.inputs["Anisotropic IOR Level"].default_value = 0.0
    elif "Anisotropic" in principled.inputs:
        principled.inputs["Anisotropic"].default_value = 0.0
    if "Coat Weight" in principled.inputs:
        principled.inputs["Coat Weight"].default_value = 0.0
    elif "Clearcoat" in principled.inputs:
        principled.inputs["Clearcoat"].default_value = 0.0
    _link(links, principled, "BSDF", output, "Surface")

    geometry = _node(nodes, "ShaderNodeNewGeometry", "World-space position; metres", (-1500, 400))
    bevel = _node(nodes, "ShaderNodeBevel", "Layered edge normal 0.2mm", (-900, 300))
    bevel.inputs["Radius"].default_value = BEVEL_RADIUS_M
    bevel.samples = 4

    # Broad 1–3 mm mottle, with soft Voronoi cells distorted by lower-frequency noise.
    mottle_scale = 1.0 / (cell_mm / 1000.0)
    mottle_coords = _node(nodes, "ShaderNodeVectorMath", "World metres to cell coordinates", (-1250, 750))
    mottle_coords.operation = "SCALE"
    mottle_coords.inputs[3].default_value = mottle_scale
    _link(links, geometry, "Position", mottle_coords, 0)
    voronoi = _node(nodes, "ShaderNodeTexVoronoi", "Soft stonewash mottle {}mm".format(cell_mm), (-1000, 800))
    voronoi.distance = "EUCLIDEAN"
    voronoi.feature = "SMOOTH_F1"
    voronoi.inputs["Scale"].default_value = 1.0
    voronoi.inputs["Smoothness"].default_value = 0.8
    _link(links, mottle_coords, 0, voronoi, "Vector")
    mottle_noise = _node(nodes, "ShaderNodeTexNoise", "Organic low-frequency breakup", (-1000, 580))
    mottle_noise.inputs["Scale"].default_value = 1.0
    mottle_noise.inputs["Detail"].default_value = 2.0
    _link(links, mottle_coords, 0, mottle_noise, "Vector")
    mottle_mix = _node(nodes, "ShaderNodeMixRGB", "Soft irregular mottle", (-750, 720))
    mottle_mix.blend_type = "MULTIPLY"
    mottle_mix.inputs[0].default_value = 0.18
    _link(links, voronoi, "Distance", mottle_mix, 1)
    _link(links, mottle_noise, "Fac", mottle_mix, 2)
    broad_bump = _node(nodes, "ShaderNodeBump", "Subtle broad mottle relief", (-450, 520))
    broad_bump.inputs["Strength"].default_value = 0.18
    broad_bump.inputs["Distance"].default_value = 0.000025
    _link(links, mottle_mix, 0, broad_bump, "Height")
    _link(links, bevel, "Normal", broad_bump, "Normal")

    # Three faint, overlapping orientations avoid a single repeated or polygonal grain.
    masks = [_scratch_noise(nodes, links, geometry, angle, i + 1)
             for i, angle in enumerate((0.18, 1.04, 2.31))]
    scratches = masks[0]
    scratch_output = "Result"
    for index, other in enumerate(masks[1:], 1):
        maximum = _node(nodes, "ShaderNodeMath", "Overlapping scratches {}".format(index), (80, -600 - index * 90))
        maximum.operation = "MAXIMUM"
        _link(links, scratches, scratch_output, maximum, 0)
        _link(links, other, "Result", maximum, 1)
        scratches = maximum
        scratch_output = "Value"
    scratch_bump = _node(nodes, "ShaderNodeBump", "Scratched dents 60um", (600, 240))
    scratch_bump.inputs["Strength"].default_value = 0.32
    scratch_bump.inputs["Distance"].default_value = SCRATCH_DEPTH_UM / 1_000_000.0
    _link(links, scratches, scratch_output, scratch_bump, "Height")
    _link(links, broad_bump, "Normal", scratch_bump, "Normal")

    # Edge-only normal contrast gives bevels a restrained worn highlight.
    dot = _node(nodes, "ShaderNodeVectorMath", "Bevel-to-face normal contrast", (-350, 100))
    dot.operation = "DOT_PRODUCT"
    _link(links, geometry, "Normal", dot, 0)
    _link(links, bevel, "Normal", dot, 1)
    edge = _math(nodes, links, "SUBTRACT", 1.0, 0.0, (-100, 100))
    _link(links, dot, "Value", edge, 1)
    edge_mask = _node(nodes, "ShaderNodeMapRange", "Subtle worn chamfer mask", (100, 100))
    edge_mask.clamp = True
    edge_mask.inputs["From Min"].default_value = 0.015
    edge_mask.inputs["From Max"].default_value = 0.14
    edge_mask.inputs["To Min"].default_value = 0.0
    edge_mask.inputs["To Max"].default_value = 0.22
    _link(links, edge, "Value", edge_mask, "Value")
    color_mix = _node(nodes, "ShaderNodeMixRGB", "Faint brighter worn chamfers", (700, 600))
    color_mix.blend_type = "MIX"
    color_mix.inputs[1].default_value = (0.42, 0.43, 0.44, 1.0)
    color_mix.inputs[2].default_value = (0.47, 0.48, 0.49, 1.0)
    _link(links, edge_mask, "Result", color_mix, 0)
    _link(links, color_mix, 0, principled, "Base Color")
    _link(links, scratch_bump, "Normal", principled, "Normal")
    material["draft_status"] = "unapproved generic material study"
    material["cell_mm"] = cell_mm
    material["scratch_depth_um"] = SCRATCH_DEPTH_UM
    material["roughness"] = ROUGHNESS
    material["metallic"] = METALLIC
    material["bevel_radius_m"] = BEVEL_RADIUS_M
    return material


def build_library(path):
    materials = [build_material(cell_mm) for cell_mm in VARIANTS]
    expected = ["DRAFT stonewash {}mm".format(mm) for mm in VARIANTS]
    if [material.name for material in materials] != expected or not all(material.use_fake_user for material in materials):
        raise RuntimeError("stonewash library material set is incomplete")
    print("STONEWASH_LIBRARY " + ", ".join(
        "{}[cell_mm={}, roughness={}, metal={}, fake_user={}]".format(
            material.name, material["cell_mm"], material["roughness"], material["metallic"], material.use_fake_user)
        for material in materials), flush=True)
    bpy.ops.wm.save_as_mainfile(filepath=path)
