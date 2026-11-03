"""Reusable product-look materials, studio set, and still-render settings.

Call from Blender after importing knife meshes in canonical knife-space orientation
(metres, resting on z=0). This module does not import assets or create a camera.
"""
from mathutils import Vector
import bpy


def _principled(nt, **values):
    node = nt.nodes.new('ShaderNodeBsdfPrincipled')
    for key, value in values.items():
        node.inputs[key].default_value = value
    return node


def _new_material(name):
    mat = bpy.data.materials.new(name)
    mat.use_nodes = True
    nt = mat.node_tree
    nt.nodes.clear()
    return mat, nt, nt.nodes, nt.links


def _bevel_bump(nt, radius, noise_scale, strength, distance, stretch=(1, 1, 1)):
    nodes, links = nt.nodes, nt.links
    bevel = nodes.new('ShaderNodeBevel')
    bevel.samples = 4
    bevel.inputs['Radius'].default_value = radius
    tc = nodes.new('ShaderNodeTexCoord')
    mapping = nodes.new('ShaderNodeMapping')
    mapping.inputs['Scale'].default_value = stretch
    noise = nodes.new('ShaderNodeTexNoise')
    noise.inputs['Scale'].default_value = noise_scale
    noise.inputs['Detail'].default_value = 6
    noise.inputs['Roughness'].default_value = 0.6
    bump = nodes.new('ShaderNodeBump')
    bump.inputs['Strength'].default_value = strength
    bump.inputs['Distance'].default_value = distance
    links.new(tc.outputs['Object'], mapping.inputs['Vector'])
    links.new(mapping.outputs['Vector'], noise.inputs['Vector'])
    links.new(noise.outputs['Fac'], bump.inputs['Height'])
    links.new(bevel.outputs['Normal'], bump.inputs['Normal'])
    return bump.outputs['Normal'], noise.outputs['Fac']


def _steel():
    mat, nt, nodes, links = _new_material('Blade - satin stainless')
    out = nodes.new('ShaderNodeOutputMaterial')
    p = _principled(nt, **{'Base Color': (0.62, 0.62, 0.625, 1), 'Metallic': 1.0,
                           'Roughness': 0.10, 'Anisotropic': 0.25})
    normal, grain = _bevel_bump(nt, 0.00010, 1.0, 0.04, 0.00001, (40, 6000, 40))
    links.new(normal, p.inputs['Normal'])
    geom = nodes.new('ShaderNodeNewGeometry')
    transform = nodes.new('ShaderNodeVectorTransform')
    transform.vector_type = 'NORMAL'; transform.convert_from = 'WORLD'; transform.convert_to = 'OBJECT'
    links.new(geom.outputs['True Normal'], transform.inputs['Vector'])
    sep = nodes.new('ShaderNodeSeparateXYZ'); links.new(transform.outputs['Vector'], sep.inputs['Vector'])
    absolute = nodes.new('ShaderNodeMath'); absolute.operation = 'ABSOLUTE'; links.new(sep.outputs['Z'], absolute.inputs[0])
    flat = nodes.new('ShaderNodeMath'); flat.operation = 'GREATER_THAN'; flat.inputs[1].default_value = 0.995
    links.new(absolute.outputs[0], flat.inputs[0])
    rough_flat = nodes.new('ShaderNodeMapRange'); rough_flat.inputs['To Min'].default_value = 0.08; rough_flat.inputs['To Max'].default_value = 0.12
    rough_edge = nodes.new('ShaderNodeMapRange'); rough_edge.inputs['To Min'].default_value = 0.035; rough_edge.inputs['To Max'].default_value = 0.05
    for node in (rough_flat, rough_edge):
        node.inputs['From Min'].default_value = 0.35; node.inputs['From Max'].default_value = 0.65
        links.new(grain, node.inputs['Value'])
    mix = nodes.new('ShaderNodeMix'); mix.data_type = 'FLOAT'
    links.new(flat.outputs[0], mix.inputs['Factor']); links.new(rough_edge.outputs['Result'], mix.inputs['A'])
    links.new(rough_flat.outputs['Result'], mix.inputs['B']); links.new(mix.outputs['Result'], p.inputs['Roughness'])
    anis = nodes.new('ShaderNodeMix'); anis.data_type = 'FLOAT'
    anis.inputs['A'].default_value = 0.15; anis.inputs['B'].default_value = 0.25
    links.new(flat.outputs[0], anis.inputs['Factor']); links.new(anis.outputs['Result'], p.inputs['Anisotropic'])
    tangent = nodes.new('ShaderNodeCombineXYZ'); tangent.inputs['X'].default_value = 1.0
    tangent_xform = nodes.new('ShaderNodeVectorTransform'); tangent_xform.vector_type = 'VECTOR'
    tangent_xform.convert_from = 'OBJECT'; tangent_xform.convert_to = 'WORLD'
    links.new(tangent.outputs['Vector'], tangent_xform.inputs['Vector']); links.new(tangent_xform.outputs['Vector'], p.inputs['Tangent'])
    links.new(p.outputs['BSDF'], out.inputs['Surface'])
    return mat


def _anodized():
    mat, nt, nodes, links = _new_material('Handle - black anodised')
    out = nodes.new('ShaderNodeOutputMaterial')
    p = _principled(nt, **{'Base Color': (0.013, 0.0135, 0.0155, 1), 'Metallic': 0.35,
                           'Roughness': 0.30, 'Specular IOR Level': 0.55,
                           'Coat Weight': 0.25, 'Coat Roughness': 0.28})
    normal, _ = _bevel_bump(nt, 0.00012, 6500, 0.10, 0.000006)
    links.new(normal, p.inputs['Normal']); links.new(normal, p.inputs['Coat Normal'])
    links.new(p.outputs['BSDF'], out.inputs['Surface'])
    return mat


def _titanium():
    mat, nt, nodes, links = _new_material('Hardware - bead-blasted titanium')
    out = nodes.new('ShaderNodeOutputMaterial')
    p = _principled(nt, **{'Base Color': (0.50, 0.49, 0.47, 1), 'Metallic': 1.0, 'Roughness': 0.22})
    normal, _ = _bevel_bump(nt, 0.00008, 9000, 0.08, 0.000005)
    links.new(normal, p.inputs['Normal']); links.new(p.outputs['BSDF'], out.inputs['Surface'])
    return mat


def _stone():
    mat, nt, nodes, links = _new_material('Set - graphite stone')
    out = nodes.new('ShaderNodeOutputMaterial')
    p = _principled(nt, **{'Base Color': (0.007, 0.009, 0.014, 1), 'Metallic': 0.0,
                           'Roughness': 0.20, 'Specular IOR Level': 0.5})
    tc = nodes.new('ShaderNodeTexCoord'); noise = nodes.new('ShaderNodeTexNoise')
    noise.inputs['Scale'].default_value = 9.0; noise.inputs['Detail'].default_value = 4
    links.new(tc.outputs['Object'], noise.inputs['Vector'])
    rough = nodes.new('ShaderNodeMapRange')
    rough.inputs['From Min'].default_value = 0.35; rough.inputs['From Max'].default_value = 0.7
    rough.inputs['To Min'].default_value = 0.16; rough.inputs['To Max'].default_value = 0.24
    links.new(noise.outputs['Fac'], rough.inputs['Value']); links.new(rough.outputs['Result'], p.inputs['Roughness'])
    links.new(p.outputs['BSDF'], out.inputs['Surface'])
    return mat


def apply_materials(meshes):
    """Replace each imported material slot by source finish while preserving slot layout."""
    finishes = {
        'Stainless_Steel_Rough': _steel(),
        'Anodized_Aluminum_Rough_Black': _anodized(),
        'Titanium_Rough': _titanium(),
    }
    for obj in meshes:
        if obj.type != 'MESH':
            continue
        old_slots = list(obj.data.materials)
        if not old_slots or any(m is None or m.name.split('.')[0] not in finishes for m in old_slots):
            raise ValueError('Product look profile requires known finish assignments: ' + obj.name)
        replacements = [finishes[m.name.split('.')[0]] for m in old_slots]
        obj.data.materials.clear()
        for material in replacements:
            obj.data.materials.append(material)
        obj.cycles.shadow_terminator_geometry_offset = 0.15
    return finishes


def _softbox(scene, name, size, loc, aim, strength, color=(1, 1, 1), edge=0.35, vertical_grad=None):
    mesh = bpy.data.meshes.new(name)
    sx, sy = size[0] / 2, size[1] / 2
    mesh.from_pydata([(-sx, -sy, 0), (sx, -sy, 0), (sx, sy, 0), (-sx, sy, 0)], [], [(0, 1, 2, 3)])
    uv = mesh.uv_layers.new()
    for i, coord in enumerate(((0, 0), (1, 0), (1, 1), (0, 1))):
        uv.data[i].uv = coord
    obj = bpy.data.objects.new(name, mesh); scene.collection.objects.link(obj)
    obj.location = loc
    obj.rotation_euler = (Vector(aim) - Vector(loc)).to_track_quat('Z', 'Y').to_euler()
    obj.visible_camera = False; obj.visible_shadow = False; obj.visible_volume_scatter = False
    mat, nt, nodes, links = _new_material(name)
    out = nodes.new('ShaderNodeOutputMaterial'); emission = nodes.new('ShaderNodeEmission')
    emission.inputs['Color'].default_value = (*color, 1)
    tc = nodes.new('ShaderNodeTexCoord'); sep = nodes.new('ShaderNodeSeparateXYZ')
    links.new(tc.outputs['UV'], sep.inputs['Vector'])

    def feather(socket):
        lo = nodes.new('ShaderNodeMapRange'); lo.interpolation_type = 'SMOOTHERSTEP'
        lo.inputs['From Min'].default_value = 0; lo.inputs['From Max'].default_value = edge
        hi = nodes.new('ShaderNodeMapRange'); hi.interpolation_type = 'SMOOTHERSTEP'
        hi.inputs['From Min'].default_value = 1; hi.inputs['From Max'].default_value = 1 - edge
        links.new(socket, lo.inputs['Value']); links.new(socket, hi.inputs['Value'])
        minimum = nodes.new('ShaderNodeMath'); minimum.operation = 'MINIMUM'
        links.new(lo.outputs['Result'], minimum.inputs[0]); links.new(hi.outputs['Result'], minimum.inputs[1])
        return minimum.outputs[0]

    product = nodes.new('ShaderNodeMath'); product.operation = 'MULTIPLY'
    links.new(feather(sep.outputs['X']), product.inputs[0]); links.new(feather(sep.outputs['Y']), product.inputs[1])
    factor = product.outputs[0]
    if vertical_grad:
        power = nodes.new('ShaderNodeMath'); power.operation = 'POWER'; power.inputs[1].default_value = vertical_grad
        links.new(sep.outputs['Y'], power.inputs[0])
        grad = nodes.new('ShaderNodeMath'); grad.operation = 'MULTIPLY'
        links.new(factor, grad.inputs[0]); links.new(power.outputs[0], grad.inputs[1]); factor = grad.outputs[0]
    scale = nodes.new('ShaderNodeMath'); scale.operation = 'MULTIPLY'; scale.name = 'Strength'
    scale.inputs[1].default_value = strength; links.new(factor, scale.inputs[0])
    links.new(scale.outputs[0], emission.inputs['Strength']); links.new(emission.outputs['Emission'], out.inputs['Surface'])
    mesh.materials.append(mat)
    return obj


def build_set(meshes):
    """Create stone floor, world, reflection cards and linked shadow key; return lights dict."""
    scene = bpy.context.scene
    bpy.ops.mesh.primitive_plane_add(size=8, location=(0, 0, 0))
    floor = bpy.context.object; floor.name = 'Set - stone floor'; floor.data.materials.append(_stone())
    world = bpy.data.worlds.new('Black studio'); world.use_nodes = True; scene.world = world
    bg = world.node_tree.nodes['Background']
    bg.inputs['Color'].default_value = (0.0006, 0.0007, 0.0009, 1); bg.inputs['Strength'].default_value = 1.0
    aim = Vector((0, 0, 0.012)); cool, warm = (0.88, 0.94, 1.0), (1.0, 0.86, 0.72)
    specs = [
        ('Key overhead', (1.1, 0.45), (0.05, 0.12, 0.85), 3.2, cool, 0.3, None),
        ('Back scrim', (2.0, 0.9), (0.0, 0.60, 0.25), 0.30, (1, 1, 1), 0.35, 1.5),
        ('Back strip A', (0.13, 0.9), (-0.13, 0.55, 0.22), 7.5, (1, 1, 1), 0.3, None),
        ('Back strip B', (0.08, 0.9), (0.24, 0.55, 0.22), 6.0, cool, 0.3, None),
        ('Strip left', (0.07, 0.9), (-0.55, -0.05, 0.22), 45.0, cool, 0.4, None),
        ('Strip right', (0.07, 0.9), (0.55, 0.10, 0.18), 35.0, cool, 0.4, None),
        ('Rim warm', (0.9, 0.05), (-0.10, 0.55, 0.035), 30.0, warm, 0.4, None),
        ('Front fill', (1.4, 0.5), (0.0, -1.1, 0.35), 0.6, (1, 1, 1), 0.3, None),
    ]
    lights = {name: _softbox(scene, name, size, loc, aim, power, color, edge, grad)
              for name, size, loc, power, color, edge, grad in specs}
    knife_collection = bpy.data.collections.new('Product parts'); scene.collection.children.link(knife_collection)
    for obj in meshes:
        if obj.type != 'MESH':
            continue
        for collection in list(obj.users_collection):
            collection.objects.unlink(obj)
        knife_collection.objects.link(obj)
    for name in ('Back scrim', 'Back strip A', 'Back strip B', 'Strip left', 'Strip right', 'Rim warm'):
        lights[name].light_linking.receiver_collection = knife_collection
    glow = _softbox(scene, 'Backdrop glow', (3.2, 1.3), (0.0, 2.6, 0.45),
                    (0, 0, 0.45), 3.0, (0.80, 0.86, 1.0), 0.5)
    glow.visible_camera = True; glow.visible_diffuse = False
    data = bpy.data.lights.new('Shadow key', 'AREA'); data.shape = 'RECTANGLE'; data.size = 0.5
    data.size_y = 0.25; data.energy = 18; data.color = cool
    shadow = bpy.data.objects.new('Shadow key', data); scene.collection.objects.link(shadow)
    shadow.location = (0.08, 0.10, 0.75)
    shadow.rotation_euler = (aim - shadow.location).to_track_quat('-Z', 'Y').to_euler()
    shadow.visible_glossy = False
    lights['Backdrop glow'] = glow; lights['Shadow key'] = shadow
    return lights


def blade_reflection(camera, blade, knife_to_world):
    """Place a graded strip in the actual flat's specular reflection direction."""
    points = [blade.matrix_world @ v.co for v in blade.data.vertices]
    target = Vector(tuple((min(p[i] for p in points) + max(p[i] for p in points)) / 2 for i in range(3)))
    normal = (knife_to_world.to_3x3() @ Vector((0, 0, 1))).normalized()
    view = (camera.location - target).normalized()
    reflected = 2 * normal.dot(view) * normal - view
    card = _softbox(bpy.context.scene, 'Blade graded reflection', (.16, .55),
                    target + reflected * .65, target, 4.0, (.95, .97, 1.0), .48)
    receivers = bpy.data.collections.new('Blade reflection receivers')
    bpy.context.scene.collection.children.link(receivers)
    receivers.objects.link(blade)
    card.light_linking.receiver_collection = receivers
    return card


def configure_render(scene, width, height, samples=96, denoise=False):
    """Configure clean still lookdev in Cycles; leave device selection to caller."""
    render = scene.render
    render.engine = 'CYCLES'
    cycles = scene.cycles
    cycles.samples = max(96, int(samples))
    cycles.use_adaptive_sampling = False
    cycles.use_denoising = bool(denoise)
    cycles.max_bounces = 10; cycles.diffuse_bounces = 3; cycles.glossy_bounces = 8
    cycles.transmission_bounces = 2; cycles.transparent_max_bounces = 4
    cycles.caustics_reflective = False; cycles.caustics_refractive = False
    cycles.sample_clamp_indirect = 4.0; cycles.blur_glossy = 0.25; cycles.filter_width = 1.2
    render.resolution_x = int(width); render.resolution_y = int(height); render.resolution_percentage = 100
    render.use_motion_blur = False
    settings = scene.view_settings; settings.view_transform = 'AgX'; settings.exposure = 0.7
    for look in ('AgX - Medium High Contrast', 'AgX - Punchy', 'Medium High Contrast'):
        try:
            settings.look = look
            break
        except TypeError:
            continue
