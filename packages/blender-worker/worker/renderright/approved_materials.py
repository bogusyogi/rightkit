"""Apply explicit shader assets from an optional product spec."""
import os

import bpy

from .contract import ContractError


def _walk_nodes(nodes, seen=None):
    if seen is None:
        seen = set()
    for node in nodes:
        if node.bl_idname == "ShaderNodeGroup" and node.node_tree:
            if node.node_tree.library:
                raise ContractError("reviewed shader has linked node group dependency: " + node.node_tree.name)
            if node.node_tree.animation_data:
                raise ContractError("reviewed shader node groups must not use animation or drivers")
            key = node.node_tree.as_pointer()
            if key not in seen:
                seen.add(key)
                _walk_nodes(node.node_tree.nodes, seen)
        if node.bl_idname in ("ShaderNodeTexImage", "ShaderNodeTexEnvironment") and node.image:
            image = node.image
            if image.source not in ('FILE', 'GENERATED', 'TILED'):
                raise ContractError("reviewed shader uses unsupported image source: " + image.source)
            if image.library or (image.source != 'GENERATED' and not image.packed_file and not image.packed_files):
                raise ContractError("reviewed shader has unpacked image dependency: " + image.name)
        if node.bl_idname in ("ShaderNodeScript", "ShaderNodeTexIES"):
            raise ContractError("reviewed shader uses unsupported script/IES dependency")
        if getattr(node, "object", None) is not None:
            raise ContractError("reviewed shader uses an external object coordinate dependency")


def _check_material(material):
    if material.library:
        raise ContractError("reviewed material must be locally loaded: " + material.name)
    if material.animation_data or (material.node_tree and material.node_tree.animation_data):
        raise ContractError("reviewed shader must not use animation or drivers")
    if material.node_tree:
        _walk_nodes(material.node_tree.nodes)


def load_finish_material(finish, review=None):
    """Load a finish material from its explicitly selected .blend library."""
    spec = review.get("spec") if review else None
    if spec is None:
        spec = finish if "finishes" in finish else None
        finish = None if spec is not None else finish
    if spec is not None:
        finish_data = spec["finishes"][finish]
    else:
        finish_data = finish
    shader = finish_data["shader"]
    path = shader["path"]
    if not isinstance(path, str) or not os.path.isabs(path) or not os.path.isfile(path):
        raise ContractError("requested shader library must be an existing absolute path")
    material_name = shader["material"]
    with bpy.data.libraries.load(path, link=False) as (source, target):
        if material_name not in source.materials:
            raise ContractError("reviewed material is absent from library: " + material_name)
        target.materials = [material_name]
    material = target.materials[0]
    if material is None:
        raise ContractError("Blender did not load exact reviewed material: " + material_name)
    _check_material(material)
    return material


def apply_reviewed_materials(meshes, review):
    spec = review["spec"]
    parts = spec["parts"]
    mesh_objects = [obj for obj in meshes if obj.type == 'MESH']
    by_name = {obj.name: obj for obj in mesh_objects}
    if len(by_name) != len(mesh_objects) or set(by_name) != set(parts):
        raise ContractError("imported mesh names do not exactly match confirmed product parts")
    finish_materials = {}
    effective = {}
    planned = {}
    for name, part in parts.items():
        obj = by_name[name]
        current = [mat.name if mat else None for mat in obj.data.materials]
        slots = part["slots"]
        if len(current) != len(slots) or set(current) != set(slots) or any(x is None for x in current):
            raise ContractError("source material slots do not exactly match confirmed slots: " + name)
        replacements = []
        for source_name in current:
            finish_id = slots[source_name]
            if finish_id not in spec["finishes"]:
                raise ContractError("unknown reviewed finish: " + str(finish_id))
            if finish_id not in finish_materials:
                finish_materials[finish_id] = load_finish_material(finish_id, review)
            replacements.append(finish_materials[finish_id])
        planned[name] = replacements
    # Shared mesh data can have different reviewed mappings per object; split ownership.
    data_users = {}
    for obj in mesh_objects:
        data_users.setdefault(obj.data.as_pointer(), []).append(obj)
    for group in data_users.values():
        signatures = {tuple(m.name for m in planned[obj.name]) for obj in group}
        if len(signatures) > 1:
            for obj in group:
                if obj.data.users > 1:
                    obj.data = obj.data.copy()
    for name, obj in by_name.items():
        # Preserve material-slot order and polygon material indices.
        for i, mat in enumerate(planned[name]):
            obj.data.materials[i] = mat
        effective[name] = [m.name for m in planned[name]]
    return effective
