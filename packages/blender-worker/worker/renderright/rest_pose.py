"""Physical rest-pose geometry shared by product authoring paths.

Geometry math follows the archived Fenrir v2 helper, with the pivot supplied by
the caller so runtime behavior does not depend on files under ``archive/``.
"""
import bmesh
import math

from mathutils import Matrix, Vector


DENSITY = {
    'Stainless_Steel_Rough': 7.9,
    'Titanium_Rough': 4.5,
    'Anodized_Aluminum_Rough_Black': 2.7,
}


def blade_matrix(deg, pivot_mm):
    """Return blade rotation in knife space about a pivot expressed in mm."""
    pivot = Vector(pivot_mm) / 1000
    return (Matrix.Translation(pivot)
            @ Matrix.Rotation(math.radians(deg), 4, 'Z')
            @ Matrix.Translation(-pivot))


def _points(meshes, blade, deg, pivot_mm):
    points = []
    rotation = blade_matrix(deg, pivot_mm)
    for obj in meshes:
        transform = (rotation if obj is blade else Matrix()) @ obj.matrix_world
        points.extend(transform @ vertex.co for vertex in obj.data.vertices)
    return points


def centre_of_mass(meshes, blade, deg, pivot_mm, densities=None):
    """Return vertex-triangulated mesh centre of mass, preserving archive math."""
    total, accumulated = 0.0, Vector()
    rotation = blade_matrix(deg, pivot_mm)
    for obj in meshes:
        transform = (rotation if obj is blade else Matrix()) @ obj.matrix_world
        material = obj.data.materials[0].name if obj.data.materials else ''
        density = densities[obj.name] if densities is not None else DENSITY.get(material, 2.7)
        vertices = [transform @ vertex.co for vertex in obj.data.vertices]
        for polygon in obj.data.polygons:
            indices = list(polygon.vertices)
            for index in range(1, len(indices) - 1):
                a, b, c = vertices[indices[0]], vertices[indices[index]], vertices[indices[index + 1]]
                volume = a.dot(b.cross(c)) / 6.0
                total += density * volume
                accumulated += density * volume * (a + b + c) / 4.0
    return accumulated / total


def stable_rest(meshes, blade, deg, prefer=Vector((0, 0, -1)), pivot_mm=None, densities=None):
    """Find convex-hull facet physically supporting the knife, nearest ``prefer``. pivot_mm comes from the product spec."""
    if pivot_mm is None:
        raise ValueError('stable_rest requires the product pivot_mm')
    bm = bmesh.new()
    for point in _points(meshes, blade, deg, pivot_mm):
        bm.verts.new(point)
    bmesh.ops.convex_hull(bm, input=bm.verts[:])
    bmesh.ops.dissolve_limit(bm, angle_limit=math.radians(0.5), verts=bm.verts[:], edges=bm.edges[:])
    bm.faces.ensure_lookup_table()
    bm.normal_update()
    com = centre_of_mass(meshes, blade, deg, pivot_mm, densities=densities)
    best = None
    for face in bm.faces:
        normal = face.normal.normalized()
        center = face.calc_center_median()
        if normal.dot(center - com) < 0:
            normal = -normal
        projected = com - normal * normal.dot(com - face.verts[0].co)
        loops = [vertex.co for vertex in face.verts]
        sides = [((loops[(index + 1) % len(loops)] - loops[index]).cross(projected - loops[index])).dot(normal)
                 for index in range(len(loops))]
        inside = all(value >= -1e-10 for value in sides) or all(value <= 1e-10 for value in sides)
        if not inside:
            continue
        score = normal.dot(prefer)
        if best is None or score > best[0]:
            best = (score, normal.copy(), face.verts[0].co.copy(),
                    [vertex.co.copy() for vertex in face.verts], face.calc_area())
    bm.free()
    return {'normal': best[1], 'point': best[2], 'support': best[3],
            'area_mm2': best[4] * 1e6, 'com': com, 'score': best[0]}


def rest_matrix(rest, yaw_axis=Vector((1, 0, 0))):
    """Place knife on its rest facet at z=0, long axis along world X."""
    normal = rest['normal']
    rotation = normal.rotation_difference(Vector((0, 0, -1))).to_matrix().to_4x4()
    axis = (rotation @ yaw_axis.to_4d()).to_3d()
    axis.z = 0
    yaw = Matrix.Rotation(-math.atan2(axis.y, axis.x), 4, 'Z')
    transform = yaw @ rotation
    z0 = (transform @ rest['point']).z
    com = transform @ rest['com']
    return Matrix.Translation(Vector((-com.x, -com.y, -z0))) @ transform
