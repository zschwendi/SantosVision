//! Actual offscreen Metal checks of the production first-person depth merge.
//! These synthetic images qualify its GPU arithmetic and queue/resource order,
//! not the game's draw provenance or visible headset alignment.
use super::*;

fn gpu() -> Gpu {
    let mut config = wasmtime::Config::new();
    config.wasm_threads(true).shared_memory(true);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let memory = SharedMemory::new(&engine, wasmtime::MemoryType::shared(4, 4)).unwrap();
    Gpu::new(memory, 0, 65536, 0, None).unwrap()
}

fn image(g: &Gpu, width: u32, height: u32, data: &[f32]) -> Texture {
    let texture = g.device.create_texture(&TextureDescriptor {
        label: Some("foreground merge test image"),
        size: Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: TextureDimension::D2,
        format: TextureFormat::R32Float,
        usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC | TextureUsages::COPY_DST,
        view_formats: &[],
    });
    upload(g, &texture, data);
    texture
}

fn upload(g: &Gpu, texture: &Texture, data: &[f32]) {
    assert_eq!(data.len(), (texture.width() * texture.height()) as usize);
    let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
    g.queue.write_texture(TexelCopyTextureInfo {
        texture, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All,
    }, &bytes, TexelCopyBufferLayout {
        offset: 0, bytes_per_row: Some(texture.width() * 4), rows_per_image: Some(texture.height()),
    }, texture.size());
}

fn read(g: &mut Gpu, texture: &Texture) -> Vec<u32> {
    g.flush();
    let row = (texture.width() * 4 + 255) & !255;
    let buffer = g.device.create_buffer(&BufferDescriptor {
        label: Some("foreground merge test readback"), size: u64::from(row) * u64::from(texture.height()),
        usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ, mapped_at_creation: false,
    });
    let mut encoder = g.device.create_command_encoder(&CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(TexelCopyTextureInfo {
        texture, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All,
    }, TexelCopyBufferInfo { buffer: &buffer, layout: TexelCopyBufferLayout {
        offset: 0, bytes_per_row: Some(row), rows_per_image: Some(texture.height()),
    } }, texture.size());
    let (tx, rx) = std::sync::mpsc::channel();
    encoder.map_buffer_on_submit(&buffer, MapMode::Read, .., move |r| { let _ = tx.send(r); });
    let submission = g.queue.submit([encoder.finish()]);
    g.device.poll(PollType::Wait { submission_index: Some(submission), timeout: Some(Duration::from_secs(10)) }).unwrap();
    rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
    let mapped = buffer.slice(..).get_mapped_range().unwrap();
    let mut output = Vec::new();
    for y in 0..texture.height() as usize {
        for value in mapped[y * row as usize..y * row as usize + texture.width() as usize * 4].chunks_exact(4) {
            output.push(u32::from_le_bytes(value.try_into().unwrap()));
        }
    }
    drop(mapped);
    buffer.unmap();
    output
}

fn sessions() -> fg::logic::Sessions {
    fg::logic::Sessions { overlays: vec![fg::logic::Overlay {
        target: 1, identity: 1, first_generation: 1, last_generation: 2,
        families: 3, draws: 2, phase: fg::logic::Phase::Closed,
    }], ..Default::default() }
}

fn pair(target: &Texture, before: Texture, after: Texture) -> HashMap<u32, ForegroundImages> {
    HashMap::from([(1, ForegroundImages { target: target.clone(), before, after: Some(after) })])
}

fn expected(world: &[f32], before: &[f32], after: &[f32]) -> Vec<u32> {
    fg::logic::merge_reference(world, before, after).unwrap().iter().map(|v| v.to_bits()).collect()
}

#[test]
fn metal_foreground_merge_keeps_world_and_real_depth_bits_in_both_conventions() {
    let mut g = gpu();
    let (width, height) = (64, 17);
    let n = (width * height) as usize;
    for reverse in [false, true] {
        let world: Vec<_> = (0..n).map(|i| 0.2 + i as f32 / (n * 3) as f32).collect();
        let mut before: Vec<f32> = vec![if reverse { 0.0 } else { 1.0 }; n];
        before[n - 1] = 0.5;
        let mut after = before.clone();
        for i in 0..n {
            // An irregular foreground with holes, plus a one-ULP change.
            if (i % width as usize) > 5 && i % 7 != 0 { after[i] = 0.6 + (i % 9) as f32 * 0.01; }
        }
        after[n - 1] = f32::from_bits(before[n - 1].to_bits() + 1);
        let w = image(&g, width, height, &world);
        let b = image(&g, width, height, &before);
        let a = image(&g, width, height, &after);
        let mut images = pair(&a, b, a.clone());
        let (output, _) = g.merge_foreground_depth(&sessions(), &mut images, 1, &w).unwrap();
        assert_eq!(read(&mut g, &output), expected(&world, &before, &after), "reverse={reverse}");
        assert_eq!(read(&mut g, &w), world.iter().map(|v| v.to_bits()).collect::<Vec<_>>());
    }
    assert_eq!(g.errors.load(Ordering::Relaxed), 0);
}

#[test]
fn metal_foreground_pool_reuse_cannot_change_an_already_encoded_merge() {
    let mut g = gpu();
    let world = vec![0.75; 64];
    let before = vec![1.0; 64];
    let after: Vec<_> = (0..64).map(|i| if i % 2 == 0 { 0.25 } else { 1.0 }).collect();
    let w = image(&g, 8, 8, &world);
    let b = image(&g, 8, 8, &before);
    let a = image(&g, 8, 8, &after);
    let mut images = pair(&a, b, a.clone());
    let (first, _) = g.merge_foreground_depth(&sessions(), &mut images, 1, &w).unwrap();
    g.foreground_recycle(images);
    let recycled_b = g.foreground_pooled(8, 8);
    let recycled_a = g.foreground_pooled(8, 8);
    // Use encoder copies, as production does. Queue.write_texture here would
    // be ordered before the unsubmitted first merge and is deliberately wrong.
    let second_before = vec![0.0; 64];
    let second_after: Vec<_> = (0..64).map(|i| if i % 3 == 0 { 0.9 } else { 0.0 }).collect();
    let source_b = image(&g, 8, 8, &second_before);
    let source_a = image(&g, 8, 8, &second_after);
    for (source, destination) in [(&source_b, &recycled_b), (&source_a, &recycled_a)] {
        g.end_pass();
        g.encoder().copy_texture_to_texture(
            TexelCopyTextureInfo { texture: source, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All },
            TexelCopyTextureInfo { texture: destination, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All }, source.size());
    }
    let mut second_images = pair(&source_a, recycled_b, recycled_a);
    let (second, _) = g.merge_foreground_depth(&sessions(), &mut second_images, 1, &w).unwrap();
    assert_eq!(read(&mut g, &first), expected(&world, &before, &after));
    assert_eq!(read(&mut g, &second), expected(&world, &second_before, &second_after));
    assert_eq!(g.errors.load(Ordering::Relaxed), 0);
}
