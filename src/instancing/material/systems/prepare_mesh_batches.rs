use std::{
    collections::{BTreeMap, BTreeSet},
    marker::PhantomData, ops::{Deref, DerefMut},
};

use crate::prelude::{DrawIndexedIndirect, DrawIndirect};
use bevy::{
    prelude::{debug, default, info_span, Entity, Handle, Mesh, Query, Res, ResMut},
    render::mesh::Indices,
};

use crate::instancing::{
    instance_slice::InstanceSlice,
    material::{
        material_instanced::MaterialInstanced,
        plugin::{GpuIndexBufferData, GpuIndirectData, InstancedMeshKey, MeshBatch, RenderMeshes},
    },
    render::instance::Instance,
};

pub struct MeshBatches<M: MaterialInstanced> {
    pub mesh_batches: BTreeMap<InstancedMeshKey, MeshBatch>,
    _phantom: PhantomData<M>,
}

impl<M: MaterialInstanced> Deref for MeshBatches<M> {
    type Target = BTreeMap<InstancedMeshKey, MeshBatch>;

    fn deref(&self) -> &Self::Target {
        &self.mesh_batches
    }
}

impl<M: MaterialInstanced> DerefMut for MeshBatches<M> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.mesh_batches
    }
}

impl<M: MaterialInstanced> Default for MeshBatches<M> {
    fn default() -> Self {
        Self {
            mesh_batches: Default::default(),
            _phantom: Default::default(),
        }
    }
}

pub fn system<M: MaterialInstanced>(
    render_meshes: Res<RenderMeshes>,
    query_instance: Query<(
        Entity,
        &Handle<M>,
        &Handle<Mesh>,
        &<M::Instance as Instance>::ExtractedInstance,
    )>,
    query_instance_slice: Query<(Entity, &Handle<M>, &Handle<Mesh>, &InstanceSlice)>,
    mut mesh_batches: ResMut<MeshBatches<M>>,
) {
    debug!("{}", std::any::type_name::<M>());

    let render_meshes = &render_meshes.instanced_meshes;

    // Sort meshes into batches by their InstancedMeshKey
    let keyed_meshes = info_span!("Key meshes").in_scope(|| {
        let mut keyed_meshes = BTreeMap::<InstancedMeshKey, BTreeSet<Handle<Mesh>>>::new();
        for mesh_handle in query_instance
            .iter()
            .map(|(_, _, mesh, _)| mesh)
            .chain(query_instance_slice.iter().map(|(_, _, mesh, _)| mesh))
        {
            let mesh = render_meshes.get(&mesh_handle).unwrap();
            keyed_meshes
                .entry(mesh.key.clone())
                .or_default()
                .insert(mesh_handle.clone_weak());
        }
        keyed_meshes
    });

    // Generate vertex, index, and indirect data for each batch
    mesh_batches.mesh_batches = info_span!("Batch meshes").in_scope(|| {
        keyed_meshes
            .into_iter()
            .map(|(key, meshes)| {
                let vertex_data = info_span!("Vertex data").in_scope(|| {
                    meshes
                        .iter()
                        .flat_map(|mesh| render_meshes.get(mesh))
                        .flat_map(|mesh| mesh.vertex_buffer_data.iter())
                        .copied()
                        .collect::<Vec<_>>()
                });

                let mut base_index = 0;
                let index_data = info_span!("Index data").in_scope(|| {
                    meshes.iter().fold(None, |acc, mesh| {
                        let mesh = render_meshes.get(mesh).unwrap();

                        let out = match &mesh.index_buffer_data {
                            GpuIndexBufferData::Indexed {
                                indices,
                                index_count,
                                index_format,
                            } => Some(match acc {
                                Some(GpuIndexBufferData::Indexed {
                                    indices: acc_indices,
                                    index_count: acc_index_count,
                                    ..
                                }) => GpuIndexBufferData::Indexed {
                                    indices: match (acc_indices, indices) {
                                        (Indices::U16(lhs), Indices::U16(rhs)) => Indices::U16(
                                            lhs.iter()
                                                .copied()
                                                .chain(
                                                    rhs.iter().map(|idx| base_index as u16 + *idx),
                                                )
                                                .collect(),
                                        ),
                                        (Indices::U32(lhs), Indices::U32(rhs)) => Indices::U32(
                                            lhs.iter()
                                                .copied()
                                                .chain(
                                                    rhs.iter().map(|idx| base_index as u32 + *idx),
                                                )
                                                .collect(),
                                        ),
                                        _ => panic!("Mismatched index format"),
                                    },

                                    index_count: index_count + acc_index_count,
                                    index_format: *index_format,
                                },
                                None => GpuIndexBufferData::Indexed {
                                    indices: indices.clone(),
                                    index_count: *index_count,
                                    index_format: *index_format,
                                },
                                _ => panic!("Mismatched GpuIndexBufferData"),
                            }),
                            GpuIndexBufferData::NonIndexed { vertex_count } => Some(match acc {
                                Some(GpuIndexBufferData::NonIndexed {
                                    vertex_count: acc_vertex_count,
                                }) => GpuIndexBufferData::NonIndexed {
                                    vertex_count: vertex_count + acc_vertex_count,
                                },
                                None => GpuIndexBufferData::NonIndexed {
                                    vertex_count: *vertex_count,
                                },
                                _ => panic!("Mismatched GpuIndexBufferData"),
                            }),
                        };

                        base_index += mesh.vertex_count;

                        out
                    })
                });

                let mut base_index = 0u32;
                let indirect_data =
                    info_span!("Indirect data").in_scope(|| match key.index_format {
                        Some(_) => GpuIndirectData::Indexed {
                            buffer: meshes
                                .iter()
                                .map(|mesh| {
                                    match &render_meshes.get(mesh).unwrap().index_buffer_data {
                                        GpuIndexBufferData::Indexed { index_count, .. } => {
                                            base_index += index_count;

                                            DrawIndexedIndirect {
                                                vertex_count: *index_count,
                                                ..default()
                                            }
                                        }
                                        _ => panic!("Mismatched GpuIndexBufferData"),
                                    }
                                })
                                .collect::<Vec<_>>(),
                        },
                        None => GpuIndirectData::NonIndexed {
                            buffer: meshes
                                .iter()
                                .map(|mesh| {
                                    match &render_meshes.get(mesh).unwrap().index_buffer_data {
                                        GpuIndexBufferData::NonIndexed { vertex_count } => {
                                            base_index += vertex_count;

                                            DrawIndirect {
                                                vertex_count: *vertex_count,
                                                ..default()
                                            }
                                        }
                                        _ => panic!("Mismatched GpuIndexBufferData"),
                                    }
                                })
                                .collect::<Vec<_>>(),
                        },
                    });

                (
                    key.clone(),
                    MeshBatch {
                        meshes,
                        vertex_data,
                        index_data,
                        indirect_data,
                    },
                )
            })
            .collect()
    });
}
