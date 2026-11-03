"""Regenerate archived landmark reference with Blender, without changing archive files."""
import hashlib
import json
from pathlib import Path
import sys

import bpy
from mathutils import Vector

root = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(root))
from renderright import rest_pose
from renderright.editorial_look import worker_resource

bpy.ops.wm.read_factory_settings(use_empty=True)
source = Path(worker_resource('archive/fenrir-measurements/source/fenrir.obj'))
bpy.ops.wm.obj_import(filepath=str(source), global_scale=0.001, forward_axis='NEGATIVE_Z', up_axis='Y',
                      use_split_objects=True, use_split_groups=True)
meshes = [obj for obj in bpy.data.objects if obj.type == 'MESH']
blade = next(o for o in meshes if 'BLADE' in o.name)
scene_build = root / 'archive/fenrir-v2/evidence/scene-build.json'
scene = json.loads(scene_build.read_text(encoding='utf-8'))
pivot_mm = scene['pivot_mm']
support = rest_pose.stable_rest(meshes, blade, 0, pivot_mm=pivot_mm)
rest = rest_pose.rest_matrix(support)
reference = {
    'source_obj_sha256': hashlib.sha256(source.read_bytes()).hexdigest(),
    'scene_build_sha256': hashlib.sha256(scene_build.read_bytes()).hexdigest(),
    'rest_matrix': [list(row) for row in rest], 'rest_normal': list(support['normal']),
    'pivot_knife_mm': pivot_mm, 'tip_world_m': scene['tip_world'],
    'pivot_world_m': list(rest @ (Vector(pivot_mm) / 1000)),
}
destination = root / 'recipes/fenrir-registration.json'
destination.write_text(json.dumps(reference, indent=2) + '\n', encoding='utf-8', newline='\n')
print(json.dumps({'reference': str(destination), **reference}))
