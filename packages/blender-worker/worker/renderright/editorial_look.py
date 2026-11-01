"""Unreviewed photographic look development; controls describe experiments, not product facts."""
import math
import json
import os
import bpy
from mathutils import Vector, Quaternion
from .contract import ContractError

WORKER_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
with open(os.path.join(WORKER_ROOT, 'recipes', 'fenrir-editorial-draft.json'), encoding='utf-8') as preset:
    DEFAULTS = json.load(preset)['settings']
# Finish-parameter defaults (ti_*, blade_*, edge_bevel_mm) come from the sample product spec, not the draft.
with open(os.path.join(WORKER_ROOT, 'recipes', 'samples', 'fenrir-sample-product.json'), encoding='utf-8') as sample:
    DEFAULTS.update(json.load(sample)['finish_profiles']['sample_ti_stonewash']['parameters'])
ASSETS_ENV = "RIGHTKIT_BLENDER_ASSETS_DIR"


def _external_dir():
    value = os.environ.get(ASSETS_ENV, "").strip()
    return os.path.abspath(value) if value else None


def worker_resource(relative):
    """Resolve a worker-relative path such as assets/..., references/... or archive/...

    With RIGHTKIT_BLENDER_ASSETS_DIR set, <dir>/<relative> is tried first; the in-repository
    path is the fallback so an unconfigured installed worker behaves as before. A file missing
    in every location fails the job, naming the variable and the file.
    """
    parts = relative.replace("\\", "/").split("/")
    if os.path.isabs(relative) or ".." in parts:
        raise ContractError("worker resource must stay inside the external asset folder or worker: " + relative)
    external = _external_dir()
    candidates = ([os.path.join(external, relative)] if external else []) + [os.path.join(WORKER_ROOT, relative)]
    for candidate in candidates:
        if os.path.isfile(candidate):
            return candidate
    state = "set to " + external if external else "unset"
    raise ContractError("missing Blender resource '{}': looked in {}; {} is {}".format(
        relative, ", ".join(candidates), ASSETS_ENV, state))


_default_external = _external_dir()
DEFAULTS["texture_root"] = os.path.join(_default_external, "assets", "materials") if _default_external else os.path.join(WORKER_ROOT, "assets", "materials")
_DEFAULT_TEXTURE_ROOT = DEFAULTS["texture_root"]


def settings(params):
    overrides = params.get('lookdev_settings', {})
    if not isinstance(overrides, dict) or set(overrides)-set(DEFAULTS):
        raise ContractError('unknown lookdev_settings')
    values = dict(DEFAULTS, **overrides)
    if values['surface'] not in ('concrete', 'leather', 'walnut'):
        raise ContractError('draft surface must be concrete, leather or walnut')
    for key, value in values.items():
        if key not in ('surface', 'texture_root', 'finish_profile', 'blade_finish') and (isinstance(value, bool) or not isinstance(value,(int,float)) or not math.isfinite(value)):
            raise ContractError('lookdev setting must be finite: '+key)
    if not .3 <= values['occupancy'] <= .94 or not 35 <= values['lens'] <= 150 or not 1 <= values['aperture'] <= 64:
        raise ContractError('lookdev camera outside supported bounds')
    if values['texture_scale'] <= 0 or values['key_size'] <= 0:
        raise ContractError('texture scale & key size must be positive')
    if not isinstance(values['texture_root'], str):
        raise ContractError('texture_root must be a directory path')
    return values


def _texture_file(cfg, asset, name):
    primary = os.path.join(cfg['texture_root'], asset, name)
    if os.path.isfile(primary):
        return primary
    if cfg['texture_root'] != _DEFAULT_TEXTURE_ROOT:
        raise ContractError('missing texture for job texture_root: ' + primary)
    return worker_resource(os.path.join('assets', 'materials', asset, name))


def material(name, color, metal, rough):
    mat=bpy.data.materials.new(name); mat.use_nodes=True
    p=mat.node_tree.nodes.get('Principled BSDF')
    p.inputs['Base Color'].default_value=(*color,1)
    p.inputs['Metallic'].default_value=metal
    p.inputs['Roughness'].default_value=rough
    p.inputs['Coat Weight'].default_value=0
    return mat,p


def grain(mat,p,scale,strength,distance):
    n,l=mat.node_tree.nodes,mat.node_tree.links
    coord=n.new('ShaderNodeTexCoord'); mapping=n.new('ShaderNodeVectorMath'); mapping.operation='MULTIPLY'
    mapping.inputs[1].default_value=scale; l.new(coord.outputs['Object'],mapping.inputs[0])
    noise=n.new('ShaderNodeTexNoise'); noise.inputs['Scale'].default_value=1; noise.inputs['Detail'].default_value=2
    l.new(mapping.outputs[0],noise.inputs['Vector'])
    bevel=n.new('ShaderNodeBevel'); bevel.inputs['Radius'].default_value=.00006; bevel.samples=4
    bump=n.new('ShaderNodeBump'); bump.inputs['Strength'].default_value=strength; bump.inputs['Distance'].default_value=distance
    l.new(noise.outputs['Fac'],bump.inputs['Height']); l.new(bevel.outputs['Normal'],bump.inputs['Normal'])
    l.new(bump.outputs['Normal'],p.inputs['Normal'])
    return noise


def draft_materials(meshes, blade, knife_matrix, cfg, finish=None):
    if finish is not None:
        from .finish_profiles import apply_profile
        return apply_profile(meshes, blade, knife_matrix, cfg, finish)
    steel,p=material('DRAFT satin steel',(.55,.55,.53),1,cfg['blade_roughness'])
    p.inputs['Anisotropic'].default_value=cfg['blade_anisotropy']
    p.inputs['Tangent'].default_value=knife_matrix.to_3x3() @ Vector((1,0,0))
    grain(steel,p,(45,9500,45),cfg['grain_strength'],.000007)
    handle,p=material('DRAFT stonewash Ti concept',(cfg['handle_value'],)*3,1,cfg['handle_roughness'])
    grain(handle,p,(7000,7000,7000),cfg['handle_texture'],.000008)
    hardware,p=material('DRAFT neutral metal hardware',(.35,.34,.32),1,.32)
    grain(hardware,p,(5000,5000,5000),.025,.000003)
    finish={}
    for obj in meshes:
        mats=[]
        for src in obj.data.materials:
            m=steel if obj is blade else handle if src and 'Anodized' in src.name else hardware
            mats.append(m)
        for i,m in enumerate(mats): obj.data.materials[i]=m
        finish[obj.name]=[m.name for m in mats]
    return finish


def _image(path,colorspace):
    if not os.path.isfile(path): raise ContractError('missing lookdev texture: '+path)
    img=bpy.data.images.load(path,check_existing=True); img.colorspace_settings.name=colorspace
    return img


def graded_card(scene, name, size, pos, target, strength, color, gradient, edge=.35):
    """Feathered emissive card with a top-to-bottom falloff: flat facets reflect a gradient, not a white stripe."""
    from .premium_look import _softbox
    card=_softbox(scene, name, size, pos, target, strength, color, edge, gradient)
    card.visible_camera=False; card.visible_diffuse=True; card.visible_glossy=True
    return card


def build_set(scene, cfg, target, meshes=None, blade=None, knife_matrix=None):
    knife=bpy.data.collections.new('Knife receivers'); scene.collection.children.link(knife)
    for o in meshes or (): knife.objects.link(o)
    if blade is not None and cfg['blade_card']>0:
        from .premium_look import _softbox
        pts=[blade.matrix_world@v.co for v in blade.data.vertices]
        bt=Vector(tuple((min(p[i] for p in pts)+max(p[i] for p in pts))/2 for i in range(3)))
        bn=(knife_matrix.to_3x3()@Vector((0,0,1))).normalized(); view=(scene.camera.location-bt).normalized()
        refl=2*bn.dot(view)*bn-view
        sz=cfg['blade_card_size']
        card=_softbox(scene,'Blade graded reflection',(.16*sz,.55*sz),bt+refl*.65,bt,cfg['blade_card'],(1.0,cfg['blade_card_warm'],cfg['blade_card_warm']*.9),.48,cfg['blade_card_grad'] or None)
        card.visible_camera=False
        if cfg['blade_card_flip']: card.rotation_euler.rotate_axis('Z',math.pi)
        recv=bpy.data.collections.new('Blade reflection receivers'); scene.collection.children.link(recv); recv.objects.link(blade)
        card.light_linking.receiver_collection=recv
    bpy.ops.mesh.primitive_plane_add(size=4,location=(0,0,-.00003))
    floor=bpy.context.object; floor.name='Draft textured surface'
    if cfg['surface']=='walnut':
        floor.data.materials.append(walnut(cfg))
        return _finish_set(scene,cfg,target,meshes,blade,knife_matrix,knife,floor)
    mat,p=material('Draft '+cfg['surface'],(cfg['surface_value'],)*3,0,cfg['surface_roughness'])
    p.inputs['Coat Weight'].default_value=cfg['surface_coat']; p.inputs['Coat Roughness'].default_value=.35
    floor.data.materials.append(mat); n,l=mat.node_tree.nodes,mat.node_tree.links
    asset='Concrete034' if cfg['surface']=='concrete' else 'Leather028'
    color_path=_texture_file(cfg,asset,asset+'_2K-PNG_Color.png'); normal_path=_texture_file(cfg,asset,asset+'_2K-PNG_NormalGL.png')
    coord=n.new('ShaderNodeTexCoord'); mapping=n.new('ShaderNodeVectorMath'); mapping.operation='SCALE'
    mapping.inputs[3].default_value=1/cfg['texture_scale']; l.new(coord.outputs['Object'],mapping.inputs[0])
    tex=n.new('ShaderNodeTexImage'); tex.image=_image(color_path,'sRGB'); l.new(mapping.outputs[0],tex.inputs['Vector'])
    mix=n.new('ShaderNodeMixRGB'); mix.blend_type='MULTIPLY'; mix.inputs[0].default_value=1
    mix.inputs[2].default_value=(cfg['surface_value'],cfg['surface_value']*cfg['surface_g'],cfg['surface_value']*cfg['surface_b'],1)
    l.new(tex.outputs['Color'],mix.inputs[1]); l.new(mix.outputs[0],p.inputs['Base Color'])
    normal=n.new('ShaderNodeTexImage'); normal.image=_image(normal_path,'Non-Color')
    l.new(mapping.outputs[0],normal.inputs['Vector'])
    norm=n.new('ShaderNodeNormalMap'); norm.inputs['Strength'].default_value=cfg['texture_strength']
    l.new(normal.outputs['Color'],norm.inputs['Color']); l.new(norm.outputs[0],p.inputs['Normal'])
    return _finish_set(scene,cfg,target,meshes,blade,knife_matrix,knife,floor)


def _finish_set(scene,cfg,target,meshes,blade,knife_matrix,knife,floor):
    if cfg['set_light']>0:
        setcol=bpy.data.collections.new('Set receivers'); scene.collection.children.link(setcol)
        for o in scene.objects:
            if o.type=='MESH' and o not in meshes and 'card' not in o.name.lower() and not (cfg['set_skip_floor']>0 and o is floor): setcol.objects.link(o)
        data=bpy.data.lights.new('Set soft','AREA'); data.shape='RECTANGLE'; data.size=.6*cfg['set_size']; data.size_y=.9*cfg['set_size']; data.energy=cfg['set_light']; data.color=(1,.95,.9)
        sl=bpy.data.objects.new('Set soft',data); scene.collection.objects.link(sl); sl.location=(cfg['set_x'],cfg['set_y'],cfg['set_z'])
        sl.rotation_euler=(target-sl.location).to_track_quat('-Z','Y').to_euler(); sl.light_linking.receiver_collection=setcol
    world=bpy.data.worlds.new('Neutral low ambience'); world.use_nodes=True; scene.world=world
    world.node_tree.nodes['Background'].inputs['Color'].default_value=(cfg['world_r'],cfg['world_g'],cfg['world_b'],1)
    world.node_tree.nodes['Background'].inputs['Strength'].default_value=cfg['world_strength']
    if cfg['bounce']>0:
        cam=scene.camera; m=cam.matrix_world.to_3x3(); fwd=-(m@Vector((0,0,1))); up=m@Vector((0,1,0))
        graded_card(scene,'Bounce card',(1.0,1.0),cam.location-fwd*.3+up*.45,target,cfg['bounce'],(1,.96,.9),0.0,edge=.5)
    if cfg['front_bounce']>0:
        cam=scene.camera; m=cam.matrix_world.to_3x3(); right=m@Vector((1,0,0))
        graded_card(scene,'Front bounce',(.4,.4),target+right*.35+Vector((0,0,.05)),target,cfg['front_bounce'],(1,.95,.9),0.0,edge=.4)
    if cfg['neg_fill']>0:
        cam=scene.camera; m=cam.matrix_world.to_3x3(); right=m@Vector((1,0,0))
        bpy.ops.mesh.primitive_plane_add(size=cfg['neg_fill'],location=target+right*.45+Vector((0,0,.2)))
        flag=bpy.context.object; flag.name='Negative fill'; flag.rotation_euler=(target-flag.location).to_track_quat('Z','Y').to_euler()
        black,_=material('Flag black',(.005,.005,.005),0,1); flag.data.materials.append(black); flag.visible_camera=False
    for name,pos,power,size,color in (
        ('Hard accent',(cfg['accent_x'],cfg['accent_y'],cfg['accent_z']),cfg['accent_power'],cfg['accent_size'],(1,cfg['accent_warm'],cfg['accent_warm']*.86)),
        ('Rim',(cfg['rim_x'],cfg['rim_y'],cfg['rim_z']),cfg['rim_power'],cfg['rim_size'],(.9,.93,1)),
        ('Glint',(cfg['glint_x'],cfg['glint_y'],cfg['glint_z']),cfg['glint_power'],cfg['glint_size'],(1,.95,.88)),
        ('Tip pool',(target.x-.09,target.y,cfg['pool_z']),cfg['pool_power'],0.2,(1,.95,.9)),
        ('Floor wash',(cfg['floor_x'],cfg['floor_y'],cfg['floor_z']),cfg['floor_power'],0.6,(1,.93,.85)),
        ('Backdrop splash',(cfg['splash_x'],cfg['splash_y'],cfg['splash_z']),cfg['splash_power'],1.0,(1,.9,.8)),
        ('Broad soft key',(cfg['key_x'],cfg['key_y'],cfg['key_z']),cfg['key_power'],cfg['key_size'],(1,cfg['warm_key'],cfg['warm_key']*.88)),
        ('Raking side',(.3,-.13,.10),cfg['rake_power'],.20,(1,.95,.88)),
        ('Restrained cool fill',(-.1,-.5,.25),cfg['fill_power'],.45,(.80,.88,1))):
        if power<=0: continue
        if name=='Broad soft key' and cfg['key_gradient']>0:
            card=graded_card(scene,'Graded key card',(size*1.6,size*3.2),Vector(pos),target,power*cfg['key_card_gain'],color,cfg['key_gradient'])
            if cfg['key_link']>0: card.light_linking.receiver_collection=knife
            continue
        data=bpy.data.lights.new(name,'AREA'); data.energy=power; data.shape='DISK'; data.size=size; data.color=color
        if name=='Broad soft key' and cfg['key_shape']>0:
            data.shape='RECTANGLE'; data.size_y=size*cfg['key_shape']
        obj=bpy.data.objects.new(name,data); scene.collection.objects.link(obj); obj.location=pos
        obj.rotation_euler=(target-obj.location).to_track_quat('-Z','Y').to_euler()
        if name=='Rim': obj.light_linking.receiver_collection=knife
        if name=='Glint':
            bl=bpy.data.collections.new('Glint receivers'); scene.collection.children.link(bl)
            if blade is not None: bl.objects.link(blade)
            obj.light_linking.receiver_collection=bl
        if name=='Tip pool':
            tp=bpy.data.collections.new('Tip pool receivers'); scene.collection.children.link(tp)
            tp.objects.link(floor)
            for o in meshes or (): tp.objects.link(o)
            obj.light_linking.receiver_collection=tp
            obj.rotation_euler=(Vector((target.x-.09,target.y,0))-obj.location).to_track_quat('-Z','Y').to_euler()
        if name=='Floor wash': obj.rotation_euler=(Vector((cfg['floor_aim_x'],cfg['floor_aim_y'],0))-obj.location).to_track_quat('-Z','Y').to_euler()
        if name=='Backdrop splash': obj.rotation_euler=(Vector((0.4,cfg['backdrop_y'],0.6))-obj.location).to_track_quat('-Z','Y').to_euler()
    return floor


def pose_meshes(meshes, blade, rest, cfg):
    """Prop the knife: stand it on its tip (edge down, show side toward camera) & lean the handle back on a block."""
    from mathutils import Matrix
    lean=math.radians(cfg['pose_lean'])
    if lean<=0: return rest
    tip=min((rest@v.co for v in blade.data.vertices), key=lambda p:p.x)
    stand=Matrix.Rotation(math.radians(90+cfg['pose_roll']),4,'X')      # flat faces -Y, edge down
    posed=stand@rest
    tip_now=posed@(rest.inverted()@tip)
    lift=Matrix.Translation(tip_now)@Matrix.Rotation(-lean,4,'Y')@Matrix.Translation(-tip_now)
    posed=lift@posed
    back=Matrix.Translation(tip_now)@Matrix.Rotation(-math.radians(cfg['pose_back']),4,'X')@Matrix.Translation(-tip_now)
    posed=back@posed
    if cfg['pose_yaw']:
        yawm=Matrix.Translation(tip_now)@Matrix.Rotation(math.radians(cfg['pose_yaw']),4,'Z')@Matrix.Translation(-tip_now)
        posed=yawm@posed
    points=[posed@(rest.inverted()@(rest@v.co)) for o in meshes for v in o.data.vertices]
    low=min(p.z for p in points)
    posed=Matrix.Translation((0,0,-low))@posed
    for o in meshes: o.matrix_world=posed
    bpy.context.view_layer.update()
    return posed


def walnut(cfg):
    """Dark oiled walnut: wave grain distorted by noise, warm albedo, low roughness with grain-driven variation."""
    v=cfg['surface_value']
    mat,p=material('Draft dark walnut',(v,v*.54,v*.32),0,.3)
    p.inputs['Coat Weight'].default_value=.15; p.inputs['Coat Roughness'].default_value=.45
    n,l=mat.node_tree.nodes,mat.node_tree.links
    coord=n.new('ShaderNodeTexCoord'); rot=n.new('ShaderNodeVectorRotate'); rot.rotation_type='AXIS_ANGLE'; rot.inputs['Axis'].default_value=(0,0,1); rot.inputs['Angle'].default_value=math.radians(cfg['grain_angle'])
    l.new(coord.outputs['Object'],rot.inputs['Vector'])
    wave=n.new('ShaderNodeTexWave'); wave.wave_type='BANDS'; wave.bands_direction='Y'; wave.inputs['Scale'].default_value=9; wave.inputs['Distortion'].default_value=2.5; wave.inputs['Detail'].default_value=2; wave.inputs['Detail Scale'].default_value=1.5
    l.new(rot.outputs['Vector'],wave.inputs['Vector'])
    fine=n.new('ShaderNodeTexNoise'); fine.inputs['Scale'].default_value=400; fine.inputs['Detail'].default_value=2; l.new(rot.outputs['Vector'],fine.inputs['Vector'])
    pores=n.new('ShaderNodeVectorMath'); pores.operation='MULTIPLY'; pores.inputs[1].default_value=(1,12,1); l.new(rot.outputs['Vector'],pores.inputs[0])
    pn=n.new('ShaderNodeTexNoise'); pn.inputs['Scale'].default_value=180; pn.inputs['Detail'].default_value=3; pn.inputs['Roughness'].default_value=.7; l.new(pores.outputs[0],pn.inputs['Vector'])
    fig=n.new('ShaderNodeTexNoise'); fig.inputs['Scale'].default_value=22; fig.inputs['Detail'].default_value=3; l.new(rot.outputs['Vector'],fig.inputs['Vector'])
    fig2=n.new('ShaderNodeTexNoise'); fig2.inputs['Scale'].default_value=37; fig2.inputs['Detail'].default_value=3; l.new(coord.outputs['Object'],fig2.inputs['Vector'])
    figsum=n.new('ShaderNodeMath'); figsum.operation='ADD'; l.new(fig.outputs['Fac'],figsum.inputs[0]); l.new(fig2.outputs['Fac'],figsum.inputs[1])
    figm=n.new('ShaderNodeMapRange'); figm.inputs['From Min'].default_value=.7; figm.inputs['From Max'].default_value=1.3; figm.inputs['To Min'].default_value=.93; figm.inputs['To Max'].default_value=1.07; l.new(figsum.outputs[0],figm.inputs['Value'])
    ramp=n.new('ShaderNodeValToRGB'); ramp.color_ramp.elements[0].color=(v*.8,v*.42,v*.24,1); ramp.color_ramp.elements[1].color=(v*1.15,v*.64,v*.38,1)
    l.new(wave.outputs['Fac'],ramp.inputs['Fac'])
    dark=n.new('ShaderNodeMixRGB'); dark.blend_type='MULTIPLY'; dark.inputs[0].default_value=.35; l.new(ramp.outputs['Color'],dark.inputs[1]); l.new(pn.outputs['Fac'],dark.inputs[2])
    figc=n.new('ShaderNodeMixRGB'); figc.blend_type='MULTIPLY'; figc.inputs[0].default_value=1; l.new(dark.outputs[0],figc.inputs[1]); l.new(figm.outputs['Result'],figc.inputs[2])
    l.new(figc.outputs[0],p.inputs['Base Color'])
    rough=n.new('ShaderNodeMapRange'); rough.inputs['From Min'].default_value=0; rough.inputs['From Max'].default_value=1; rough.inputs['To Min'].default_value=.25; rough.inputs['To Max'].default_value=.4
    l.new(wave.outputs['Fac'],rough.inputs['Value'])
    rv=n.new('ShaderNodeTexNoise'); rv.inputs['Scale'].default_value=25; rv.inputs['Detail'].default_value=2; l.new(coord.outputs['Object'],rv.inputs['Vector'])
    rvm=n.new('ShaderNodeMath'); rvm.operation='MULTIPLY_ADD'; rvm.inputs[1].default_value=.3; rvm.inputs[2].default_value=-.15; l.new(rv.outputs['Fac'],rvm.inputs[0])
    radd=n.new('ShaderNodeMath'); radd.operation='ADD'; l.new(rough.outputs['Result'],radd.inputs[0]); l.new(rvm.outputs[0],radd.inputs[1]); radd.use_clamp=True
    l.new(radd.outputs[0],p.inputs['Roughness'])
    bump=n.new('ShaderNodeBump'); bump.inputs['Strength'].default_value=.25; bump.inputs['Distance'].default_value=.0001
    l.new(pn.outputs['Fac'],bump.inputs['Height']); l.new(bump.outputs['Normal'],p.inputs['Normal'])
    return mat


def oxide_tray(name, value):
    """Black-oxide steel with brushed anisotropy, chipped paint showing rust (face) & bare steel (edges), smudge scuffs."""
    mat,p=material(name,(value,value*1.02,value*1.05),.85,.42)
    p.inputs['Anisotropic'].default_value=0.0; p.inputs['Specular IOR Level'].default_value=.5
    n,l=mat.node_tree.nodes,mat.node_tree.links
    coord=n.new('ShaderNodeTexCoord')
    geo=n.new('ShaderNodeNewGeometry'); bevel=n.new('ShaderNodeBevel'); bevel.inputs['Radius'].default_value=.0018; bevel.samples=6
    dot=n.new('ShaderNodeVectorMath'); dot.operation='DOT_PRODUCT'; l.new(geo.outputs['True Normal'],dot.inputs[0]); l.new(bevel.outputs['Normal'],dot.inputs[1])
    edge=n.new('ShaderNodeMapRange'); edge.clamp=True; edge.inputs['From Min'].default_value=.998; edge.inputs['From Max'].default_value=.95; l.new(dot.outputs['Value'],edge.inputs['Value'])
    chips=n.new('ShaderNodeTexNoise'); chips.inputs['Scale'].default_value=45; chips.inputs['Detail'].default_value=5; chips.inputs['Roughness'].default_value=.85; l.new(coord.outputs['Object'],chips.inputs['Vector'])
    chipmask=n.new('ShaderNodeMapRange'); chipmask.clamp=True; chipmask.inputs['From Min'].default_value=.60; chipmask.inputs['From Max'].default_value=.63; l.new(chips.outputs['Fac'],chipmask.inputs['Value'])
    edgewear=n.new('ShaderNodeMath'); edgewear.operation='MULTIPLY'; l.new(edge.outputs['Result'],edgewear.inputs[0]); l.new(chipmask.outputs['Result'],edgewear.inputs[1])
    fc=n.new('ShaderNodeTexNoise'); fc.inputs['Scale'].default_value=55; fc.inputs['Detail'].default_value=8; fc.inputs['Roughness'].default_value=.9; l.new(coord.outputs['Object'],fc.inputs['Vector'])
    facechip=n.new('ShaderNodeMapRange'); facechip.clamp=True; facechip.inputs['From Min'].default_value=.66; facechip.inputs['From Max'].default_value=.69; l.new(fc.outputs['Fac'],facechip.inputs['Value'])
    rustcol=n.new('ShaderNodeMixRGB'); rustcol.inputs[1].default_value=(value,value*1.02,value*1.05,1); rustcol.inputs[2].default_value=(.18,.085,.045,1)
    l.new(facechip.outputs['Result'],rustcol.inputs[0])
    barecol=n.new('ShaderNodeMixRGB'); barecol.inputs[2].default_value=(.22,.215,.2,1); l.new(edgewear.outputs[0],barecol.inputs[0]); l.new(rustcol.outputs[0],barecol.inputs[1])
    l.new(barecol.outputs[0],p.inputs['Base Color'])
    metal=n.new('ShaderNodeMath'); metal.operation='MULTIPLY_ADD'; metal.inputs[1].default_value=-.85; metal.inputs[2].default_value=.85; l.new(facechip.outputs['Result'],metal.inputs[0])
    l.new(metal.outputs[0],p.inputs['Metallic'])
    smudge=n.new('ShaderNodeTexNoise'); smudge.inputs['Scale'].default_value=18; smudge.inputs['Detail'].default_value=3; smudge.inputs['Roughness'].default_value=.6; l.new(coord.outputs['Object'],smudge.inputs['Vector'])
    rough=n.new('ShaderNodeMapRange'); rough.inputs['From Min'].default_value=.35; rough.inputs['From Max'].default_value=.65; rough.inputs['To Min'].default_value=.4; rough.inputs['To Max'].default_value=.5
    l.new(smudge.outputs['Fac'],rough.inputs['Value'])
    rough2=n.new('ShaderNodeMath'); rough2.operation='MULTIPLY_ADD'; rough2.inputs[1].default_value=.3; l.new(facechip.outputs['Result'],rough2.inputs[0]); l.new(rough.outputs['Result'],rough2.inputs[2]); rough2.use_clamp=True
    # scratch field: two stretched-noise directions, roughness +-.15 & shallow grooves
    scr=None
    for sx,sy in ((1,30),(26,1.4)):
        st=n.new('ShaderNodeVectorMath'); st.operation='MULTIPLY'; st.inputs[1].default_value=(sx*45,sy*45,45); l.new(coord.outputs['Object'],st.inputs[0])
        sn=n.new('ShaderNodeTexNoise'); sn.inputs['Scale'].default_value=1; sn.inputs['Detail'].default_value=2; sn.inputs['Roughness'].default_value=.8; l.new(st.outputs[0],sn.inputs['Vector'])
        sm=n.new('ShaderNodeMapRange'); sm.clamp=True; sm.inputs['From Min'].default_value=.6; sm.inputs['From Max'].default_value=.74; l.new(sn.outputs['Fac'],sm.inputs['Value'])
        if scr is None: scr=sm.outputs['Result']
        else:
            mx=n.new('ShaderNodeMath'); mx.operation='MAXIMUM'; l.new(scr,mx.inputs[0]); l.new(sm.outputs['Result'],mx.inputs[1]); scr=mx.outputs[0]
    # edge highlight break-up: roughness varies along the edge
    en=n.new('ShaderNodeTexNoise'); en.inputs['Scale'].default_value=220; en.inputs['Detail'].default_value=2; l.new(coord.outputs['Object'],en.inputs['Vector'])
    er=n.new('ShaderNodeMath'); er.operation='MULTIPLY'; l.new(edge.outputs['Result'],er.inputs[0]); l.new(en.outputs['Fac'],er.inputs[1])
    rough3=n.new('ShaderNodeMath'); rough3.operation='MULTIPLY_ADD'; rough3.inputs[1].default_value=-.3; l.new(scr,rough3.inputs[0]); l.new(rough2.outputs[0],rough3.inputs[2])
    rough4=n.new('ShaderNodeMath'); rough4.operation='MULTIPLY_ADD'; rough4.inputs[1].default_value=.4; l.new(er.outputs[0],rough4.inputs[0]); l.new(rough3.outputs[0],rough4.inputs[2]); rough4.use_clamp=True
    l.new(rough4.outputs[0],p.inputs['Roughness'])
    # paint wear albedo +-15% at 30-80 mm & dust specks
    pw=n.new('ShaderNodeTexNoise'); pw.inputs['Scale'].default_value=18; pw.inputs['Detail'].default_value=3; l.new(coord.outputs['Object'],pw.inputs['Vector'])
    pwm=n.new('ShaderNodeMapRange'); pwm.inputs['From Min'].default_value=.3; pwm.inputs['From Max'].default_value=.7; pwm.inputs['To Min'].default_value=.7; pwm.inputs['To Max'].default_value=1.35; l.new(pw.outputs['Fac'],pwm.inputs['Value'])
    dust=n.new('ShaderNodeTexNoise'); dust.inputs['Scale'].default_value=900; dust.inputs['Detail'].default_value=1; l.new(coord.outputs['Object'],dust.inputs['Vector'])
    dm=n.new('ShaderNodeMapRange'); dm.clamp=True; dm.inputs['From Min'].default_value=.74; dm.inputs['From Max'].default_value=.8; dm.inputs['To Min'].default_value=1; dm.inputs['To Max'].default_value=2.2; l.new(dust.outputs['Fac'],dm.inputs['Value'])
    wearcol=n.new('ShaderNodeMixRGB'); wearcol.blend_type='MULTIPLY'; wearcol.inputs[0].default_value=1; l.new(barecol.outputs[0],wearcol.inputs[1]); l.new(pwm.outputs['Result'],wearcol.inputs[2])
    dustcol=n.new('ShaderNodeMixRGB'); dustcol.blend_type='MULTIPLY'; dustcol.inputs[0].default_value=1; l.new(wearcol.outputs[0],dustcol.inputs[1]); l.new(dm.outputs['Result'],dustcol.inputs[2])
    l.new(dustcol.outputs[0],p.inputs['Base Color'])
    dents=n.new('ShaderNodeTexNoise'); dents.inputs['Scale'].default_value=7; dents.inputs['Detail'].default_value=2; l.new(coord.outputs['Object'],dents.inputs['Vector'])
    dentm=n.new('ShaderNodeMapRange'); dentm.clamp=True; dentm.inputs['From Min'].default_value=.58; dentm.inputs['From Max'].default_value=.72; l.new(dents.outputs['Fac'],dentm.inputs['Value'])
    bump=n.new('ShaderNodeBump'); bump.inputs['Strength'].default_value=.5; bump.inputs['Distance'].default_value=.0006; l.new(dentm.outputs['Result'],bump.inputs['Height']); l.new(bevel.outputs['Normal'],bump.inputs['Normal'])
    sbump=n.new('ShaderNodeBump'); sbump.inputs['Strength'].default_value=.6; sbump.inputs['Distance'].default_value=.00006; l.new(scr,sbump.inputs['Height']); l.new(bump.outputs['Normal'],sbump.inputs['Normal']); bump=sbump
    chipbump=n.new('ShaderNodeBump'); chipbump.inputs['Strength'].default_value=.6; chipbump.inputs['Distance'].default_value=.00006; chipbump.invert=True
    l.new(facechip.outputs['Result'],chipbump.inputs['Height']); l.new(bump.outputs['Normal'],chipbump.inputs['Normal']); l.new(chipbump.outputs['Normal'],p.inputs['Normal'])
    return mat


def worn_paint(name, value, bare=.35):
    """Painted steel with bare metal breaking through on edges & a dust/roughness breakup."""
    mat,p=material(name,(value*.94,value,value*1.06),.2,.55)
    p.inputs['Specular IOR Level'].default_value=.4
    n,l=mat.node_tree.nodes,mat.node_tree.links
    coord=n.new('ShaderNodeTexCoord')
    geo=n.new('ShaderNodeNewGeometry'); bevel=n.new('ShaderNodeBevel'); bevel.inputs['Radius'].default_value=.0015; bevel.samples=6
    dot=n.new('ShaderNodeVectorMath'); dot.operation='DOT_PRODUCT'; l.new(geo.outputs['True Normal'],dot.inputs[0]); l.new(bevel.outputs['Normal'],dot.inputs[1])
    edge=n.new('ShaderNodeMapRange'); edge.clamp=True; edge.inputs['From Min'].default_value=.997; edge.inputs['From Max'].default_value=.96
    edge.inputs['To Min'].default_value=0; edge.inputs['To Max'].default_value=1; l.new(dot.outputs['Value'],edge.inputs['Value'])
    chips=n.new('ShaderNodeTexNoise'); chips.inputs['Scale'].default_value=90; chips.inputs['Detail'].default_value=6; chips.inputs['Roughness'].default_value=.85
    l.new(coord.outputs['Object'],chips.inputs['Vector'])
    chipmask=n.new('ShaderNodeMapRange'); chipmask.clamp=True; chipmask.inputs['From Min'].default_value=.42; chipmask.inputs['From Max'].default_value=.5
    l.new(chips.outputs['Fac'],chipmask.inputs['Value'])
    wear=n.new('ShaderNodeMath'); wear.operation='MULTIPLY'; l.new(edge.outputs['Result'],wear.inputs[0]); l.new(chipmask.outputs['Result'],wear.inputs[1])
    facechips=n.new('ShaderNodeTexNoise'); facechips.inputs['Scale'].default_value=12; facechips.inputs['Detail'].default_value=7; facechips.inputs['Roughness'].default_value=.85
    l.new(coord.outputs['Object'],facechips.inputs['Vector'])
    facemask=n.new('ShaderNodeMapRange'); facemask.clamp=True; facemask.inputs['From Min'].default_value=.70; facemask.inputs['From Max'].default_value=.74
    l.new(facechips.outputs['Fac'],facemask.inputs['Value'])
    wear2=n.new('ShaderNodeMath'); wear2.operation='MAXIMUM'; l.new(wear.outputs[0],wear2.inputs[0]); l.new(facemask.outputs['Result'],wear2.inputs[1]); wear=wear2
    col=n.new('ShaderNodeMixRGB'); col.inputs[1].default_value=(value*.94,value,value*1.06,1); col.inputs[2].default_value=(.45,.44,.42,1)
    l.new(wear.outputs[0],col.inputs[0]); l.new(col.outputs[0],p.inputs['Base Color'])
    metal=n.new('ShaderNodeMath'); metal.operation='MULTIPLY_ADD'; metal.inputs[1].default_value=.9; metal.inputs[2].default_value=.0
    l.new(wear.outputs[0],metal.inputs[0]); l.new(metal.outputs[0],p.inputs['Metallic'])
    dust=n.new('ShaderNodeTexNoise'); dust.inputs['Scale'].default_value=25; dust.inputs['Detail'].default_value=3; l.new(coord.outputs['Object'],dust.inputs['Vector'])
    rough=n.new('ShaderNodeMapRange'); rough.inputs['From Min'].default_value=.3; rough.inputs['From Max'].default_value=.7
    rough.inputs['To Min'].default_value=.38; rough.inputs['To Max'].default_value=.55; l.new(dust.outputs['Fac'],rough.inputs['Value'])
    rough2=n.new('ShaderNodeMath'); rough2.operation='MULTIPLY_ADD'; rough2.inputs[1].default_value=-.28; l.new(wear.outputs[0],rough2.inputs[0]); l.new(rough.outputs['Result'],rough2.inputs[2])
    # scuff streaks: stretched noise in two directions, strong roughness swing & faint lightening
    scuff=None
    for i,(sx,sy) in enumerate(((1,22),(18,1.3))):
        st=n.new('ShaderNodeVectorMath'); st.operation='MULTIPLY'; st.inputs[1].default_value=(sx*30,sy*30,30); l.new(coord.outputs['Object'],st.inputs[0])
        sn=n.new('ShaderNodeTexNoise'); sn.inputs['Scale'].default_value=1; sn.inputs['Detail'].default_value=2; sn.inputs['Roughness'].default_value=.8; l.new(st.outputs[0],sn.inputs['Vector'])
        sm=n.new('ShaderNodeMapRange'); sm.clamp=True; sm.inputs['From Min'].default_value=.68; sm.inputs['From Max'].default_value=.78; l.new(sn.outputs['Fac'],sm.inputs['Value'])
        if scuff is None: scuff=sm.outputs['Result']
        else:
            mx=n.new('ShaderNodeMath'); mx.operation='MAXIMUM'; l.new(scuff,mx.inputs[0]); l.new(sm.outputs['Result'],mx.inputs[1]); scuff=mx.outputs[0]
    rough3=n.new('ShaderNodeMath'); rough3.operation='MULTIPLY_ADD'; rough3.inputs[1].default_value=-.2; l.new(scuff,rough3.inputs[0]); l.new(rough2.outputs[0],rough3.inputs[2]); rough3.use_clamp=True
    l.new(rough3.outputs[0],p.inputs['Roughness'])
    lighten=n.new('ShaderNodeMixRGB'); lighten.blend_type='ADD'; lighten.inputs[0].default_value=.025; l.new(col.outputs[0],lighten.inputs[1]); l.new(scuff,lighten.inputs[2])
    l.new(lighten.outputs[0],p.inputs['Base Color'])
    dents=n.new('ShaderNodeTexNoise'); dents.inputs['Scale'].default_value=9; dents.inputs['Detail'].default_value=4; l.new(coord.outputs['Object'],dents.inputs['Vector'])
    bump=n.new('ShaderNodeBump'); bump.inputs['Strength'].default_value=.35; bump.inputs['Distance'].default_value=.0006
    l.new(dents.outputs['Fac'],bump.inputs['Height']); l.new(bevel.outputs['Normal'],bump.inputs['Normal']); l.new(bump.outputs['Normal'],p.inputs['Normal'])
    return mat


def prop_block(scene, meshes, cfg):
    """Worn dark box behind the knife; its front face touches the handle's back flat at the box top-edge height; yawed so its top edge runs diagonal."""
    if cfg['prop']<=0: return None
    points=[o.matrix_world@v.co for o in meshes for v in o.data.vertices]
    w,h,d=cfg['prop_w'],cfg['prop_h'],cfg['prop_d']
    yaw=math.radians(cfg['prop_yaw'])
    normal=Vector((math.sin(yaw),-math.cos(yaw),0))          # front face normal, toward camera
    below=[p for p in points if p.z<=h] or points
    contact=max(below,key=lambda p:p.dot(-normal))             # knife point furthest from camera along the face normal
    along=Vector((math.cos(yaw),math.sin(yaw),0))
    center=contact-normal*(cfg['prop_gap']+d/2)+along*cfg['prop_y']
    bpy.ops.mesh.primitive_cube_add(size=1,location=(center.x,center.y,h/2))
    block=bpy.context.object; block.name='Prop block'; block.scale=(w,d,h); block.rotation_euler=(0,0,yaw)
    bpy.ops.object.transform_apply(scale=True)
    bev=block.modifiers.new('edge','BEVEL'); bev.width=cfg['prop_lip']; bev.segments=4
    block.data.materials.append(oxide_tray('Prop oxide steel tray',cfg['prop_value']) if cfg['prop_oxide']>0 else worn_paint('Prop worn painted steel',cfg['prop_value']))
    return block


def foreground(scene, cfg):
    """Out-of-focus dark element near the lens, bottom-left, for a depth layer."""
    if cfg['fg']<=0: return None
    cam=scene.camera; m=cam.matrix_world
    fwd=-(m.to_3x3()@Vector((0,0,1))); right=m.to_3x3()@Vector((1,0,0)); up=m.to_3x3()@Vector((0,1,0))
    pos=cam.location+fwd*cfg['fg_dist']+right*cfg['fg_x']+up*cfg['fg_y']
    bpy.ops.mesh.primitive_cube_add(size=1,location=pos)
    fg=bpy.context.object; fg.name='Foreground bokeh'; fg.scale=(.11,.07,.06); fg.rotation_euler=(0,0,math.radians(30))
    bpy.ops.object.transform_apply(scale=True)
    bev=fg.modifiers.new('edge','BEVEL'); bev.width=.004; bev.segments=3
    fg.data.materials.append(oxide_tray('Foreground oxide steel',cfg['prop_value']*.8) if cfg['prop_oxide']>0 else worn_paint('Foreground worn steel',cfg['prop_value']*.8))
    fg.visible_shadow=False
    return fg


def backdrop(scene, cfg, target):
    """Far dark textured wall so the background holds a gradient instead of void."""
    if cfg['backdrop']<=0: return None
    bpy.ops.mesh.primitive_plane_add(size=4,location=(0,cfg['backdrop_y'],1.0),rotation=(math.pi/2,0,0))
    wall=bpy.context.object; wall.name='Backdrop'
    mat,p=material('Backdrop dark plaster',(cfg['backdrop'],)*3,0,.9)
    n,l=mat.node_tree.nodes,mat.node_tree.links
    coord=n.new('ShaderNodeTexCoord'); noise=n.new('ShaderNodeTexNoise'); noise.inputs['Scale'].default_value=6; noise.inputs['Detail'].default_value=8
    l.new(coord.outputs['Object'],noise.inputs['Vector'])
    mix=n.new('ShaderNodeMixRGB'); mix.blend_type='MULTIPLY'; mix.inputs[0].default_value=.9; mix.inputs[1].default_value=(cfg['backdrop'],cfg['backdrop']*.9,cfg['backdrop']*.8,1)
    l.new(noise.outputs['Fac'],mix.inputs[2]); l.new(mix.outputs[0],p.inputs['Base Color'])
    wall.data.materials.append(mat)
    return wall


def link_set_receivers(scene, meshes):
    """Props created after the set light join its receiver collection (everything that is not the knife or a card)."""
    setcol=bpy.data.collections.get('Set receivers')
    if setcol is None: return
    for o in scene.objects:
        if o.type=='MESH' and o not in meshes and 'card' not in o.name.lower() and o.name not in setcol.objects:
            setcol.objects.link(o)


def framed_meshes(meshes, cfg):
    """frame_part 0 = whole product; 1 = the profile's detail_parts (handle detail); 2 = blade only."""
    part=int(cfg['frame_part'])
    if part==1:
        keep=set(cfg.get('finish_detail_parts') or ())
        return [o for o in meshes if o.name in keep] or meshes
    if part==2:
        return [o for o in meshes if 'BLADE' in o.name.upper()] or meshes
    return meshes


def camera(scene, meshes, cfg):
    from .authoring import _projection
    meshes=framed_meshes(meshes, cfg)
    points=[o.matrix_world@v.co for o in meshes for v in o.data.vertices]
    low=Vector(tuple(min(p[i] for p in points) for i in range(3)))
    high=Vector(tuple(max(p[i] for p in points) for i in range(3)))
    target=(low+high)/2
    a,e=math.radians(cfg['azimuth']),math.radians(cfg['elevation'])
    direction=Vector((math.sin(a)*math.cos(e),-math.cos(a)*math.cos(e),math.sin(e)))
    data=bpy.data.cameras.new('Draft editorial camera'); data.lens=cfg['lens']; data.clip_start=.001
    obj=bpy.data.objects.new(data.name,data); scene.collection.objects.link(obj); scene.camera=obj
    distance=.6
    for _ in range(8):
        obj.location=target+direction*distance
        q=(target-obj.location).to_track_quat('-Z','Y')
        obj.rotation_euler=(q@Quaternion((0,0,1),math.radians(cfg['roll']))).to_euler()
        bpy.context.view_layer.update()
        uv=_projection(scene,meshes)['bounds_uv']; span=max(b-a for a,b in uv)
        distance *= span/cfg['occupancy']
    data.shift_x=cfg['shift_x']; data.shift_y=cfg['shift_y']
    focus=target+Vector((cfg['focus_bias'],0,0))
    if cfg['focus_pivot']>0:
        pivot=next((o for o in bpy.data.objects if o.name==cfg.get('finish_pivot_object')),None)
        if pivot is not None:
            pts=[pivot.matrix_world@v.co for v in pivot.data.vertices]; focus=sum(pts,Vector())/len(pts)
    data.dof.use_dof=True; data.dof.aperture_fstop=cfg['aperture']; data.dof.aperture_blades=9
    data.dof.focus_distance=(obj.location-focus).length
    return obj,target


def grade(scene,cfg):
    scene.view_settings.exposure=cfg['exposure']
    # Optical lens falloff, applied in compositor; no geometry or finish claims.
    tree=bpy.data.node_groups.new('Draft restrained vignette','CompositorNodeTree')
    scene.compositing_node_group=tree
    tree.interface.new_socket(name='Image',in_out='OUTPUT',socket_type='NodeSocketColor')
    nodes,links=tree.nodes,tree.links
    image=nodes.new('CompositorNodeRLayers')
    ellipse=nodes.new('CompositorNodeEllipseMask'); ellipse.width=.95; ellipse.height=.95
    blur=nodes.new('CompositorNodeBlur'); blur.inputs['Size'].default_value=(250,250)
    links.new(ellipse.outputs['Mask'],blur.inputs['Image'])
    ramp=nodes.new('ShaderNodeMapRange'); ramp.inputs['To Min'].default_value=1-cfg['vignette']; ramp.inputs['To Max'].default_value=1
    links.new(blur.outputs[0],ramp.inputs['Value'])
    mix=nodes.new('ShaderNodeMixRGB'); mix.blend_type='MULTIPLY'; mix.inputs[0].default_value=1
    links.new(image.outputs['Image'],mix.inputs[1]); links.new(ramp.outputs[0],mix.inputs[2])
    out=nodes.new('NodeGroupOutput'); links.new(mix.outputs[0],out.inputs['Image'])
    scene.render.use_compositing=True
