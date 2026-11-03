"""Register a product's qualified GLB into its declared knife space and physical rest pose.

Every constant comes from the product spec's ``registration`` block (see
``product_registration.py``); a spec without one falls back to the Fenrir sample registration.
"""
import bpy, hashlib, os, math
import numpy as np
from mathutils import Matrix, Vector
from . import rest_pose
from .contract import ContractError
from .product_registration import resolve_registration, check_source_sha


def _points(meshes):
    return [o.matrix_world @ v.co for o in meshes for v in o.data.vertices]
def _bounds(points):
    return [[min(p[i] for p in points), max(p[i] for p in points)] for i in range(3)]
def _rest_matrix(meshes, blade, common, pivot_mm, densities=None):
    support = common.stable_rest(meshes, blade, 0, pivot_mm=tuple(pivot_mm), densities=densities)
    return common.rest_matrix(support), support
def _tip_local(blade, band_mm):
    p = np.array([tuple(v.co) for v in blade.data.vertices])
    x = p[:,0].min()
    return Vector(p[p[:,0] < x + band_mm / 1000].mean(axis=0))
def _bore_center(blade, pivot_xy, ring_mm):
    p = np.array([tuple(v.co) for v in blade.data.vertices])
    radial = np.linalg.norm(p[:,:2] - np.array(pivot_xy), axis=1)
    lo, hi = ring_mm[0] / 1000, ring_mm[1] / 1000
    ring = p[(radial > lo) & (radial < hi)]
    if len(ring) < 20: raise RuntimeError('Could not independently locate blade bore rim')
    a = np.c_[ring[:,0], ring[:,1], np.ones(len(ring))]
    c = np.linalg.lstsq(a, -(ring[:,0]**2 + ring[:,1]**2), rcond=None)[0]
    center = Vector((-c[0]/2, -c[1]/2))
    radius = math.sqrt((c[0]**2 + c[1]**2)/4-c[2])
    return center, radius, len(ring)

def import_and_rest(source_path, product_spec=None):
    with open(os.path.abspath(source_path), 'rb') as f: digest = hashlib.sha256(f.read()).hexdigest()
    reg, reg_source = resolve_registration(product_spec)
    check_source_sha(reg, digest, 'the product spec registration' if reg_source == 'product_spec' else 'the sample registration')
    glb_to_knife = Matrix(reg['glb_to_knife'])
    pivot_mm = reg['pivot_knife_mm']
    tol = reg['tolerances']
    before = set(bpy.data.objects)
    bpy.ops.import_scene.gltf(filepath=os.path.abspath(source_path))
    meshes = [o for o in bpy.data.objects if o not in before and o.type == 'MESH']
    if not meshes: raise RuntimeError('GLB imported no mesh objects')
    imported_world = {o: o.matrix_world.copy() for o in meshes}
    for o in meshes:
        o.parent = None
        o.data.transform(glb_to_knife @ imported_world[o])
        o.matrix_world = Matrix.Identity(4)
    marker = reg['blade_marker'].upper()
    blade = next((o for o in meshes if marker in o.name.upper()), None)
    if blade is None: raise RuntimeError('GLB has no identifiable blade object (marker ' + reg['blade_marker'] + ')')
    per_object = {o.name: _bounds([v.co.copy() for v in o.data.vertices]) for o in meshes}
    part_map = reg['part_map']
    densities = None
    mass_model = 'Legacy material-label density assumptions; actual product materials remain unconfirmed.'
    if isinstance(product_spec, dict) and isinstance(product_spec.get('parts'), dict):
        parts = product_spec['parts']
        mapped = {o.name: part_map.get(o.name, o.name) for o in meshes}
        if set(parts) != set(mapped.values()):
            raise ValueError('Confirmed product parts do not match imported meshes')
        densities = {name: parts[part]['density_g_cm3'] for name, part in mapped.items()}
        mass_model = 'Confirmed product-spec substrate densities'
    report = {'source': os.path.abspath(source_path), 'source_sha256': digest,
              'registration': {'source': reg_source, 'source_sha256_declared': reg.get('source_sha256') is not None,
                               'archive_obj_sha256': reg.get('archive_obj_sha256')},
              'registration_matrix': [list(row) for row in glb_to_knife],
              'registration_derivation': reg.get('derivation', 'Registration matrix, pivot and bore are declared in the product spec.'),
              'mass_model': mass_model,
              'coordinate_space': reg.get('knife_space_label', 'product knife space, metres'),
              'mesh_count': len(meshes), 'blade': blade.name,
              'canonical_bounds_m': _bounds(_points(meshes)), 'canonical_object_bounds_m': per_object}
    rest, support = _rest_matrix(meshes, blade, rest_pose, pivot_mm, densities=densities)
    report['rest'] = {'normal': list(support['normal']), 'support_point_m': list(support['point']),
                      'com_m': list(support['com']), 'area_mm2': support['area_mm2']}
    pivot_xy = (pivot_mm[0] / 1000, pivot_mm[1] / 1000)
    c, bore_r, bore_n = _bore_center(blade, pivot_xy, reg['bore']['ring_mm'])
    # Declared analytic axis, moved to the blade's mid-plane datum.
    bore = reg['bore']
    source_axis_origin_mm = Vector(bore['axis_origin_mm'])
    source_axis_datum_mm = bore['axis_datum_mm']
    source_point = source_axis_origin_mm + Vector((0, source_axis_datum_mm, 0))
    analytic = glb_to_knife @ (source_point / 1000)
    bore_res = (c - analytic.xy).length * 1000
    axis = glb_to_knife.to_3x3() @ Vector((0,1,0))
    c1 = {'bore_circle_vertex_count': bore_n, 'fitted_radius_mm': bore_r*1000,
          'fitted_bore_centroid_xy_m': list(c), 'analytic_axis_xy_m': list(analytic.xy),
          'source_axis_origin_mm': list(source_axis_origin_mm), 'source_axis_datum_mm': source_axis_datum_mm,
          'source_to_import_units': .001, 'bore_datum_knife_z_m': analytic.z,
          'bore_centroid_residual_mm': bore_res, 'axis_direction_mapped': list(axis),
          'pass': bore_res <= tol['residual_mm'] and (axis.normalized()-Vector((0,0,1))).length <= tol['axis_direction']}
    current_tip = rest @ _tip_local(blade, reg['tip_band_mm']).to_4d()
    pivot_world = rest @ Vector((c.x, c.y, analytic.z))
    na = support['normal'].normalized()
    ref = reg.get('archive_reference')
    if ref is not None:
        archive_tip = Vector(ref['tip_world_m'])
        tip_res = (Vector(current_tip[:3])-archive_tip).length*1000
        pivot_res = (pivot_world - Vector(ref['pivot_world_m'])).length * 1000
        nb = Vector(ref['rest_normal']).normalized()
        normal_deg = math.degrees(math.atan2(na.cross(nb).length, na.dot(nb)))
        c2 = {'current_tip_rest_m': list(current_tip)[:3], 'archive_tip_rest_m': list(archive_tip),
              'pivot_is_registration_input': True,
              'independent_holdouts': ['blade tip vertices', 'support facet normal'],
              'tip_rest_residual_mm': tip_res, 'current_support_normal': list(na),
              'current_pivot_rest_m': list(pivot_world), 'archive_pivot_rest_m': ref['pivot_world_m'],
              'pivot_rest_residual_mm': pivot_res, 'archive_reference': reg_source + ' registration.archive_reference',
              'archive_support_normal': list(nb), 'rest_normal_disagreement_deg': normal_deg,
              'pass': tip_res <= tol['residual_mm'] and pivot_res <= tol['residual_mm'] and normal_deg <= tol['normal_deg']}
    else:
        c2 = {'pass': False, 'reason': 'registration declares no archive_reference; C2 cannot be checked',
              'current_support_normal': list(na), 'current_pivot_rest_m': list(pivot_world)}
    report['C1'], report['C2'] = c1, c2
    report['camera_paths_reusable'] = c1['pass'] and c2['pass']
    report['knife_to_world_matrix'] = [list(row) for row in rest]
    for o in meshes: o.matrix_world = rest
    return meshes, blade, rest, report
