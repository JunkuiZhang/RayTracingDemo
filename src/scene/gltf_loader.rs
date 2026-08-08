use std::{
    collections::HashMap,
    fmt, fs,
    path::{Path, PathBuf},
};

use glam::{Mat4, Quat, Vec3};
use gltf::{
    image::{Data as ImageData, Format as ImageFormat},
    mesh::Mode,
    scene::Transform,
};

use super::{
    FilterMode, ImageAsset, MaterialAsset, MaterialKind, MeshPrimitive, RigidAnimationGroup,
    SamplerKey, SceneAsset, SceneInstance, TextureBindingAsset, VertexAsset, WrapMode,
};

const MAX_VERTICES: usize = 4_000_000;
const MAX_INDICES: usize = 12_000_000;
const MAX_INSTANCES: usize = 100_000;
const MAX_IMAGE_BYTES: usize = 512 * 1024 * 1024;

struct ImportState<'a> {
    buffers: &'a [gltf::buffer::Data],
    materials: &'a [MaterialAsset],
    path: &'a Path,
    default_material: usize,
    primitive_map: HashMap<(usize, usize), usize>,
    primitives: Vec<MeshPrimitive>,
    instances: Vec<SceneInstance>,
}

#[derive(Debug)]
pub struct GltfLoadError {
    path: PathBuf,
    message: String,
}

impl GltfLoadError {
    fn new(path: &Path, message: impl Into<String>) -> Self {
        Self {
            path: path.to_path_buf(),
            message: message.into(),
        }
    }
}

impl fmt::Display for GltfLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}：{}", self.path.display(), self.message)
    }
}

impl std::error::Error for GltfLoadError {}

pub fn load(path: impl AsRef<Path>) -> Result<SceneAsset, GltfLoadError> {
    let path = path.as_ref();
    if !path.is_file() {
        return Err(GltfLoadError::new(path, "文件不存在或不是文件"));
    }
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension)
            if extension.eq_ignore_ascii_case("gltf") || extension.eq_ignore_ascii_case("glb") => {}
        _ => return Err(GltfLoadError::new(path, "只支持 .gltf 和 .glb 文件")),
    }

    let raw_document = gltf::Gltf::from_slice_without_validation(
        &fs::read(path).map_err(|error| GltfLoadError::new(path, format!("读取文件：{error}")))?,
    )
    .map_err(|error| GltfLoadError::new(path, format!("读取 glTF JSON/GLB：{error}")))?;
    let raw_required = raw_document.extensions_required().collect::<Vec<_>>();
    if !raw_required.is_empty() {
        return Err(GltfLoadError::new(
            path,
            format!(
                "不支持的 required glTF extension：{}",
                raw_required.join(", ")
            ),
        ));
    }

    let (document, buffers, imported_images) = gltf::import(path)
        .map_err(|error| GltfLoadError::new(path, format!("读取 glTF：{error}")))?;
    let required = document.extensions_required().collect::<Vec<_>>();
    if !required.is_empty() {
        return Err(GltfLoadError::new(
            path,
            format!("不支持的 required glTF extension：{}", required.join(", ")),
        ));
    }
    if let Some(animation) = document.animations().next() {
        return Err(GltfLoadError::new(
            path,
            format!(
                "不支持 glTF animation channel，animation index {}",
                animation.index()
            ),
        ));
    }
    if let Some(skin) = document.skins().next() {
        return Err(GltfLoadError::new(
            path,
            format!("不支持 glTF skin，skin index {}", skin.index()),
        ));
    }
    for node in document.nodes() {
        if let Some(skin) = node.skin() {
            return Err(GltfLoadError::new(
                path,
                format!("不支持 node {} 的 skin {}", node.index(), skin.index()),
            ));
        }
    }

    let mut samplers = vec![SamplerKey::default()];
    let mut materials = load_materials(&document, path, &mut samplers)?;
    let default_material = materials.len();
    materials.push(MaterialAsset::opaque("glTF default material", [1.0; 4]));
    let images = load_images(&imported_images, path)?;
    let scene = document
        .default_scene()
        .or_else(|| document.scenes().next())
        .ok_or_else(|| GltfLoadError::new(path, "glTF 没有 scene，无法确定默认场景"))?;

    let mut state = ImportState {
        buffers: &buffers,
        materials: &materials,
        path,
        default_material,
        primitive_map: HashMap::new(),
        primitives: Vec::new(),
        instances: Vec::new(),
    };
    for node in scene.nodes() {
        visit_node(&mut state, node, Mat4::IDENTITY)?;
    }
    let primitives = state.primitives;
    let mut instances = state.instances;
    if primitives.is_empty() || instances.is_empty() {
        return Err(GltfLoadError::new(
            path,
            "默认 scene 没有可渲染的 mesh node",
        ));
    }
    if primitives
        .iter()
        .map(|primitive| primitive.vertices.len())
        .sum::<usize>()
        > MAX_VERTICES
    {
        return Err(GltfLoadError::new(
            path,
            format!("顶点数超过上限 {MAX_VERTICES}"),
        ));
    }
    if primitives
        .iter()
        .map(|primitive| primitive.indices.len())
        .sum::<usize>()
        > MAX_INDICES
    {
        return Err(GltfLoadError::new(
            path,
            format!("index 数超过上限 {MAX_INDICES}"),
        ));
    }
    if instances.len() > MAX_INSTANCES {
        return Err(GltfLoadError::new(
            path,
            format!("实例数 {} 超过上限 {MAX_INSTANCES}", instances.len()),
        ));
    }

    let (placement, source_pivot) = calculate_placement(&primitives, &instances, path)?;
    let pivot_world = placement.transform_point3(source_pivot).to_array();
    for instance in &mut instances {
        let world = placement * instance.base_world;
        instance.base_world = world;
        instance.current_world = world;
        instance.previous_world = world;
    }
    let animated_instance_indices = (0..instances.len()).collect();
    let scene_asset = SceneAsset {
        primitives,
        materials,
        images,
        samplers,
        instances,
        rigid_animation_groups: vec![RigidAnimationGroup {
            instance_indices: animated_instance_indices,
            pivot_world,
        }],
    };
    scene_asset
        .validate()
        .map_err(|error| GltfLoadError::new(path, format!("验证导入场景：{error}")))?;
    Ok(scene_asset)
}

fn load_materials(
    document: &gltf::Document,
    path: &Path,
    samplers: &mut Vec<SamplerKey>,
) -> Result<Vec<MaterialAsset>, GltfLoadError> {
    let mut materials = Vec::new();
    for material in document.materials() {
        let alpha_mode = material.alpha_mode();
        if !matches!(alpha_mode, gltf::material::AlphaMode::Opaque) {
            return Err(GltfLoadError::new(
                path,
                format!(
                    "材质 {} 使用不支持的 alpha mode {:?}，只接受 OPAQUE",
                    material.index().unwrap_or(0),
                    alpha_mode
                ),
            ));
        }
        let pbr = material.pbr_metallic_roughness();
        let normal_texture_info = material.normal_texture();
        let normal_scale = normal_texture_info
            .as_ref()
            .map_or(1.0, gltf::material::NormalTexture::scale);
        let material_index = material.index().unwrap_or(0);
        let base_color_texture = pbr
            .base_color_texture()
            .map(|info| texture_binding(info, samplers, path, material_index, "base_color"))
            .transpose()?;
        let metallic_roughness_texture = pbr
            .metallic_roughness_texture()
            .map(|info| texture_binding(info, samplers, path, material_index, "metallic_roughness"))
            .transpose()?;
        let normal_texture = normal_texture_info
            .as_ref()
            .map(|info| {
                texture_binding_with_texcoord(
                    info.texture(),
                    info.tex_coord(),
                    samplers,
                    path,
                    material_index,
                    "normal",
                )
            })
            .transpose()?;
        let emissive_texture = material
            .emissive_texture()
            .map(|info| texture_binding(info, samplers, path, material_index, "emissive"))
            .transpose()?;
        let asset = MaterialAsset {
            name: material.name().unwrap_or("glTF material").to_string(),
            base_color_factor: pbr.base_color_factor(),
            metallic_factor: pbr.metallic_factor(),
            roughness_factor: pbr.roughness_factor(),
            normal_scale,
            emissive_factor: material.emissive_factor(),
            ior: 1.5,
            kind: MaterialKind::Opaque,
            double_sided: material.double_sided(),
            base_color_texture,
            metallic_roughness_texture,
            normal_texture,
            emissive_texture,
        };
        materials.push(asset);
    }
    Ok(materials)
}

fn texture_binding(
    info: gltf::texture::Info<'_>,
    samplers: &mut Vec<SamplerKey>,
    path: &Path,
    material_index: usize,
    role: &str,
) -> Result<TextureBindingAsset, GltfLoadError> {
    texture_binding_with_texcoord(
        info.texture(),
        info.tex_coord(),
        samplers,
        path,
        material_index,
        role,
    )
}

fn texture_binding_with_texcoord(
    texture: gltf::texture::Texture<'_>,
    texcoord_set: u32,
    samplers: &mut Vec<SamplerKey>,
    path: &Path,
    material_index: usize,
    role: &str,
) -> Result<TextureBindingAsset, GltfLoadError> {
    if texcoord_set != 0 {
        return Err(GltfLoadError::new(
            path,
            format!(
                "材质 {material_index} 的 {role} texture 使用不支持的 texCoord {texcoord_set}，本阶段只支持 TEXCOORD_0"
            ),
        ));
    }
    let sampler = sampler_key(texture.sampler());
    let sampler_index = if let Some(index) = samplers.iter().position(|key| *key == sampler) {
        index
    } else {
        if samplers.len() >= super::MAX_SCENE_SAMPLERS {
            return Err(GltfLoadError::new(
                path,
                format!(
                    "材质 {material_index} 的 {role} 需要超过 {} 个 sampler descriptor",
                    super::MAX_SCENE_SAMPLERS
                ),
            ));
        }
        samplers.push(sampler);
        samplers.len() - 1
    };
    Ok(TextureBindingAsset {
        image_index: texture.source().index(),
        sampler_index,
        texcoord_set,
    })
}

fn sampler_key(sampler: gltf::texture::Sampler<'_>) -> SamplerKey {
    let min_filter = match sampler.min_filter() {
        Some(gltf::texture::MinFilter::Nearest)
        | Some(gltf::texture::MinFilter::NearestMipmapNearest)
        | Some(gltf::texture::MinFilter::NearestMipmapLinear) => FilterMode::Nearest,
        Some(gltf::texture::MinFilter::Linear)
        | Some(gltf::texture::MinFilter::LinearMipmapNearest)
        | Some(gltf::texture::MinFilter::LinearMipmapLinear)
        | None => FilterMode::Linear,
    };
    let mag_filter = match sampler.mag_filter() {
        Some(gltf::texture::MagFilter::Nearest) => FilterMode::Nearest,
        Some(gltf::texture::MagFilter::Linear) | None => FilterMode::Linear,
    };
    let wrap = |mode| match mode {
        gltf::texture::WrappingMode::ClampToEdge => WrapMode::Clamp,
        gltf::texture::WrappingMode::MirroredRepeat => WrapMode::Mirror,
        gltf::texture::WrappingMode::Repeat => WrapMode::Repeat,
    };
    SamplerKey {
        min_filter,
        mag_filter,
        wrap_u: wrap(sampler.wrap_s()),
        wrap_v: wrap(sampler.wrap_t()),
    }
}

fn load_images(
    imported_images: &[ImageData],
    path: &Path,
) -> Result<Vec<ImageAsset>, GltfLoadError> {
    let mut total_bytes = 0usize;
    imported_images
        .iter()
        .enumerate()
        .map(|(index, image)| {
            let rgba8 = image_to_rgba8(image)
                .map_err(|error| GltfLoadError::new(path, format!("image {index}：{error}")))?;
            total_bytes = total_bytes.saturating_add(rgba8.len());
            if total_bytes > MAX_IMAGE_BYTES {
                return Err(GltfLoadError::new(
                    path,
                    format!("解码后 image 总字节数超过上限 {MAX_IMAGE_BYTES}"),
                ));
            }
            Ok(ImageAsset {
                name: format!("image {index}"),
                width: image.width,
                height: image.height,
                rgba8,
            })
        })
        .collect()
}

fn image_to_rgba8(image: &ImageData) -> Result<Vec<u8>, String> {
    let (channels, bytes_per_channel) = match image.format {
        ImageFormat::R8 => (1, 1),
        ImageFormat::R8G8 => (2, 1),
        ImageFormat::R8G8B8 => (3, 1),
        ImageFormat::R8G8B8A8 => (4, 1),
        ImageFormat::R16 => (1, 2),
        ImageFormat::R16G16 => (2, 2),
        ImageFormat::R16G16B16 => (3, 2),
        ImageFormat::R16G16B16A16 => (4, 2),
        ImageFormat::R32G32B32FLOAT => (3, 4),
        ImageFormat::R32G32B32A32FLOAT => (4, 4),
    };
    let pixel_count = (image.width as usize)
        .checked_mul(image.height as usize)
        .ok_or("尺寸溢出")?;
    let expected = pixel_count
        .checked_mul(channels)
        .and_then(|value| value.checked_mul(bytes_per_channel))
        .ok_or("尺寸溢出")?;
    if image.pixels.len() != expected {
        return Err(format!(
            "像素数据长度 {} 与预期 {} 不符",
            image.pixels.len(),
            expected
        ));
    }
    let mut rgba8 = vec![0u8; pixel_count * 4];
    for pixel in 0..pixel_count {
        for channel in 0..4 {
            let source_channel = if channels == 2 {
                usize::from(channel == 3)
            } else {
                channel.min(channels - 1)
            };
            let offset = (pixel * channels + source_channel) * bytes_per_channel;
            rgba8[pixel * 4 + channel] = if channel == 3 && channels != 2 && channels < 4 {
                255
            } else {
                match bytes_per_channel {
                    1 => image.pixels[offset],
                    2 => {
                        let value =
                            u16::from_le_bytes([image.pixels[offset], image.pixels[offset + 1]]);
                        (u32::from(value) + 128).div_euclid(257).min(255) as u8
                    }
                    4 => {
                        (f32::from_le_bytes(image.pixels[offset..offset + 4].try_into().unwrap())
                            .clamp(0.0, 1.0)
                            * 255.0
                            + 0.5) as u8
                    }
                    _ => unreachable!(),
                }
            };
        }
    }
    Ok(rgba8)
}

fn visit_node(
    state: &mut ImportState<'_>,
    node: gltf::Node<'_>,
    parent_world: Mat4,
) -> Result<(), GltfLoadError> {
    let local_world = convert_node_transform(node.transform());
    if !is_finite_invertible(local_world) {
        return Err(GltfLoadError::new(
            state.path,
            format!("node {} 的 TRS/matrix 非有限或不可逆", node.index()),
        ));
    }
    let world = parent_world * local_world;
    if !is_finite_invertible(world) {
        return Err(GltfLoadError::new(
            state.path,
            format!(
                "node {} 的累计 world transform 非有限或不可逆",
                node.index()
            ),
        ));
    }
    if let Some(mesh) = node.mesh() {
        for primitive in mesh.primitives() {
            if primitive.mode() != Mode::Triangles {
                return Err(GltfLoadError::new(
                    state.path,
                    format!(
                        "mesh {} primitive {} 使用不支持的 mode {:?}，只接受 TRIANGLES",
                        mesh.index(),
                        primitive.index(),
                        primitive.mode()
                    ),
                ));
            }
            if primitive.morph_targets().next().is_some() {
                return Err(GltfLoadError::new(
                    state.path,
                    format!(
                        "mesh {} primitive {} 使用不支持的 morph target",
                        mesh.index(),
                        primitive.index()
                    ),
                ));
            }
            let key = (mesh.index(), primitive.index());
            let primitive_index = if let Some(&index) = state.primitive_map.get(&key) {
                index
            } else {
                let material_index = primitive
                    .material()
                    .index()
                    .unwrap_or(state.default_material);
                let value = read_primitive(
                    &primitive,
                    state.buffers,
                    material_index,
                    state.path,
                    mesh.index(),
                )?;
                validate_texture_uvs(
                    &value,
                    &state.materials[material_index],
                    state.path,
                    mesh.index(),
                    primitive.index(),
                )?;
                let index = state.primitives.len();
                state.primitive_map.insert(key, index);
                state.primitives.push(value);
                index
            };
            let stable_id = state.instances.len() as u32;
            state.instances.push(SceneInstance {
                stable_id,
                primitive_index,
                base_world: world,
                current_world: world,
                previous_world: world,
            });
        }
    }
    for child in node.children() {
        visit_node(state, child, world)?;
    }
    Ok(())
}

fn read_primitive(
    primitive: &gltf::Primitive<'_>,
    buffers: &[gltf::buffer::Data],
    material_index: usize,
    path: &Path,
    mesh_index: usize,
) -> Result<MeshPrimitive, GltfLoadError> {
    let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()].0));
    let source_positions = reader
        .read_positions()
        .ok_or_else(|| {
            GltfLoadError::new(
                path,
                format!(
                    "mesh {mesh_index} primitive {} 缺少 POSITION",
                    primitive.index()
                ),
            )
        })?
        .collect::<Vec<_>>();
    if source_positions.is_empty() {
        return Err(GltfLoadError::new(
            path,
            format!(
                "mesh {mesh_index} primitive {} 的 POSITION 为空",
                primitive.index()
            ),
        ));
    }
    let positions = source_positions
        .into_iter()
        .map(convert_position)
        .collect::<Vec<_>>();
    let mut indices: Vec<u32> = reader
        .read_indices()
        .map(|indices| indices.into_u32().collect())
        .unwrap_or_else(|| (0..positions.len() as u32).collect());
    if !indices.len().is_multiple_of(3) {
        return Err(GltfLoadError::new(
            path,
            format!(
                "mesh {mesh_index} primitive {} 的非索引/索引顶点数不是三角形倍数",
                primitive.index()
            ),
        ));
    }
    if let Some(&index) = indices
        .iter()
        .find(|&&index| index as usize >= positions.len())
    {
        return Err(GltfLoadError::new(
            path,
            format!(
                "mesh {mesh_index} primitive {} 的 index {index} 越界",
                primitive.index()
            ),
        ));
    }
    for triangle in indices.chunks_exact_mut(3) {
        triangle.swap(1, 2);
    }

    let normals = if let Some(source_normals) = reader.read_normals() {
        let normals = source_normals.map(convert_normal).collect::<Vec<_>>();
        if normals.len() != positions.len() {
            return Err(GltfLoadError::new(
                path,
                format!(
                    "mesh {mesh_index} primitive {} 的 NORMAL 数量与 POSITION 不一致",
                    primitive.index()
                ),
            ));
        }
        normals
    } else {
        generate_normals(&positions, &indices, path, mesh_index, primitive.index())
    };
    let texcoords = reader
        .read_tex_coords(0)
        .map(|coords| {
            coords
                .into_f32()
                .map(|uv| [uv[0], uv[1]])
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let has_texcoords = if texcoords.is_empty() {
        false
    } else if texcoords.len() == positions.len() {
        true
    } else {
        return Err(GltfLoadError::new(
            path,
            format!(
                "mesh {mesh_index} primitive {} 的 TEXCOORD_0 数量与 POSITION 不一致",
                primitive.index()
            ),
        ));
    };
    let tangents = reader
        .read_tangents()
        .map(|values| values.map(convert_tangent).collect::<Vec<_>>());
    if tangents
        .as_ref()
        .is_some_and(|values| values.len() != positions.len())
    {
        return Err(GltfLoadError::new(
            path,
            format!(
                "mesh {mesh_index} primitive {} 的 TANGENT 数量与 POSITION 不一致",
                primitive.index()
            ),
        ));
    }
    let has_tangent = tangents.is_some();
    if !has_tangent && material_index < usize::MAX {
        eprintln!(
            "警告：{} mesh {} primitive {} 缺少 TANGENT；如材质有 normal map 将禁用该 primitive 的 normal map",
            path.display(),
            mesh_index,
            primitive.index()
        );
    }
    let vertices = positions
        .into_iter()
        .enumerate()
        .map(|(index, position)| VertexAsset {
            position,
            normal: normals[index],
            tangent: tangents
                .as_ref()
                .map_or([1.0, 0.0, 0.0, 1.0], |values| values[index]),
            texcoord0: if has_texcoords {
                texcoords[index]
            } else {
                [0.0; 2]
            },
            has_tangent,
        })
        .collect();
    Ok(MeshPrimitive {
        name: format!("mesh {mesh_index} primitive {}", primitive.index()),
        vertices,
        indices,
        material_index,
        has_texcoord0: has_texcoords,
    })
}

fn validate_texture_uvs(
    primitive: &MeshPrimitive,
    material: &MaterialAsset,
    path: &Path,
    mesh_index: usize,
    primitive_index: usize,
) -> Result<(), GltfLoadError> {
    if primitive.has_texcoord0 {
        return Ok(());
    }
    let Some((role, _)) = [
        ("base_color", material.base_color_texture),
        ("metallic_roughness", material.metallic_roughness_texture),
        ("normal", material.normal_texture),
        ("emissive", material.emissive_texture),
    ]
    .into_iter()
    .find(|(_, binding)| binding.is_some()) else {
        return Ok(());
    };
    Err(GltfLoadError::new(
        path,
        format!(
            "mesh {mesh_index} primitive {primitive_index} 的材质 {role} 引用纹理但缺少 TEXCOORD_0"
        ),
    ))
}

fn generate_normals(
    positions: &[[f32; 3]],
    indices: &[u32],
    path: &Path,
    mesh_index: usize,
    primitive_index: usize,
) -> Vec<[f32; 3]> {
    let mut accumulated = vec![[0.0; 3]; positions.len()];
    let mut degenerate = 0usize;
    for triangle in indices.chunks_exact(3) {
        let a = Vec3::from_array(positions[triangle[0] as usize]);
        let b = Vec3::from_array(positions[triangle[1] as usize]);
        let c = Vec3::from_array(positions[triangle[2] as usize]);
        let normal = (b - a).cross(c - a);
        if normal.length_squared() <= 1.0e-20 {
            degenerate += 1;
            continue;
        }
        for &index in triangle {
            let value = &mut accumulated[index as usize];
            *value = (Vec3::from_array(*value) + normal).to_array();
        }
    }
    if degenerate != 0 {
        eprintln!(
            "警告：{} mesh {} primitive {} 跳过 {} 个退化三角形以生成法线",
            path.display(),
            mesh_index,
            primitive_index,
            degenerate
        );
    }
    accumulated
        .into_iter()
        .map(|normal| {
            let value = Vec3::from_array(normal);
            if value.length_squared() <= 1.0e-20 {
                [0.0, 1.0, 0.0]
            } else {
                value.normalize().to_array()
            }
        })
        .collect()
}

fn calculate_placement(
    primitives: &[MeshPrimitive],
    instances: &[SceneInstance],
    path: &Path,
) -> Result<(Mat4, Vec3), GltfLoadError> {
    let mut minimum = Vec3::splat(f32::INFINITY);
    let mut maximum = Vec3::splat(f32::NEG_INFINITY);
    for instance in instances {
        for vertex in &primitives[instance.primitive_index].vertices {
            let position = instance
                .base_world
                .transform_point3(Vec3::from_array(vertex.position));
            minimum = minimum.min(position);
            maximum = maximum.max(position);
        }
    }
    if !minimum.is_finite() || !maximum.is_finite() || !minimum.cmple(maximum).all() {
        return Err(GltfLoadError::new(path, "场景 AABB 非有限或为空"));
    }
    let extent = maximum - minimum;
    let largest_extent = extent.max_element();
    if !largest_extent.is_finite() || largest_extent <= f32::EPSILON {
        return Err(GltfLoadError::new(
            path,
            "场景 AABB 尺寸为零，无法放入 Cornell Box",
        ));
    }
    let scale = 1.5 / largest_extent;
    let center = (minimum + maximum) * 0.5;
    let translation = Vec3::new(
        -center.x * scale,
        -1.0 - minimum.y * scale + 0.001,
        1.0 - center.z * scale,
    );
    Ok((
        Mat4::from_translation(translation) * Mat4::from_scale(Vec3::splat(scale)),
        center,
    ))
}

fn convert_node_transform(transform: Transform) -> Mat4 {
    let matrix = match transform {
        Transform::Matrix { matrix } => Mat4::from_cols_array_2d(&matrix),
        Transform::Decomposed {
            translation,
            rotation,
            scale,
        } => Mat4::from_scale_rotation_translation(
            Vec3::from_array(scale),
            Quat::from_array(rotation),
            Vec3::from_array(translation),
        ),
    };
    let conversion = Mat4::from_scale(Vec3::new(1.0, 1.0, -1.0));
    conversion * matrix * conversion
}

fn convert_position(position: [f32; 3]) -> [f32; 3] {
    [position[0], position[1], -position[2]]
}

fn convert_normal(normal: [f32; 3]) -> [f32; 3] {
    let value = Vec3::new(normal[0], normal[1], -normal[2]);
    if value.length_squared() <= 1.0e-20 {
        [0.0, 1.0, 0.0]
    } else {
        value.normalize().to_array()
    }
}

fn convert_tangent(tangent: [f32; 4]) -> [f32; 4] {
    let value = Vec3::new(tangent[0], tangent[1], -tangent[2]);
    let value = if value.length_squared() <= 1.0e-20 {
        Vec3::X
    } else {
        value.normalize()
    };
    [value.x, value.y, value.z, -tangent[3]]
}

fn is_finite_invertible(matrix: Mat4) -> bool {
    matrix.to_cols_array().iter().all(|value| value.is_finite())
        && matrix.determinant().abs() > f32::EPSILON
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets/gltf")
            .join(name)
    }

    #[test]
    fn loads_indexed_triangle_and_applies_coordinate_conversion() {
        let scene = load(fixture("Triangle/Triangle.gltf")).unwrap();
        assert_eq!(scene.primitives.len(), 1);
        assert_eq!(scene.instances.len(), 1);
        assert_eq!(scene.primitives[0].indices, [0, 2, 1]);
        assert_eq!(scene.primitives[0].vertices[2].position[2], -1.0);
        scene.validate().unwrap();
    }

    #[test]
    fn loads_non_indexed_parent_child_and_shared_mesh_fixture() {
        let scene = load(fixture("Triangle/NonIndexedMultiNode.gltf")).unwrap();
        assert_eq!(
            scene.primitives.len(),
            1,
            "同一 mesh primitive 必须共享 CPU 数据"
        );
        assert_eq!(scene.instances.len(), 2);
        let first = scene.instances[0].base_world.transform_point3(Vec3::ZERO);
        let second = scene.instances[1].base_world.transform_point3(Vec3::ZERO);
        assert!((first.x - second.x).abs() > 0.1 || (first.y - second.y).abs() > 0.1);
        assert!(
            scene.primitives[0]
                .vertices
                .iter()
                .all(|vertex| !vertex.has_tangent)
        );
    }

    #[test]
    fn rejects_required_extension_and_alpha_blend() {
        let required = fixture("Triangle/UnsupportedRequiredExtension.gltf");
        let error = load(&required).unwrap_err().to_string();
        assert!(error.contains("required glTF extension"));
        let alpha = fixture("Triangle/AlphaBlend.gltf");
        let error = load(&alpha).unwrap_err().to_string();
        assert!(error.contains("alpha mode"));
    }

    #[test]
    fn image_conversion_preserves_rgba_and_expands_missing_channels() {
        let image = ImageData {
            pixels: vec![12, 34, 56],
            format: ImageFormat::R8G8B8,
            width: 1,
            height: 1,
        };
        assert_eq!(image_to_rgba8(&image).unwrap(), [12, 34, 56, 255]);
    }

    #[test]
    fn image_conversion_maps_luma_alpha_without_channel_swizzle() {
        let r8 = ImageData {
            pixels: vec![42],
            format: ImageFormat::R8,
            width: 1,
            height: 1,
        };
        assert_eq!(image_to_rgba8(&r8).unwrap(), [42, 42, 42, 255]);

        let r8g8 = ImageData {
            pixels: vec![42, 200],
            format: ImageFormat::R8G8,
            width: 1,
            height: 1,
        };
        assert_eq!(image_to_rgba8(&r8g8).unwrap(), [42, 42, 42, 200]);

        let r16g16 = ImageData {
            pixels: vec![0x80, 0x80, 0xff, 0xff],
            format: ImageFormat::R16G16,
            width: 1,
            height: 1,
        };
        assert_eq!(image_to_rgba8(&r16g16).unwrap(), [128, 128, 128, 255]);
    }

    #[test]
    fn matrix_conversion_wraps_translation_rotation_and_scale() {
        let transform = Transform::Decomposed {
            translation: [1.0, 2.0, 3.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [2.0, 3.0, 4.0],
        };
        let matrix = convert_node_transform(transform);
        let point = matrix.transform_point3(Vec3::ZERO);
        assert_eq!(point, Vec3::new(1.0, 2.0, -3.0));
        assert_eq!(matrix.transform_vector3(Vec3::X), Vec3::new(2.0, 0.0, 0.0));
    }

    #[test]
    fn missing_file_has_context() {
        let error = load("assets/gltf/does-not-exist.glb").unwrap_err();
        assert!(error.to_string().contains("文件不存在"));
        let _ = fs::canonicalize(error.path).ok();
    }
}
