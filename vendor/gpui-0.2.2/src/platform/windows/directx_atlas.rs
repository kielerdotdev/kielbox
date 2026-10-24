use collections::FxHashMap;
use etagere::BucketedAtlasAllocator;
use parking_lot::Mutex;
use windows::Win32::Graphics::{
    Direct3D11::{
        D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
        ID3D11Device, ID3D11DeviceContext, ID3D11ShaderResourceView, ID3D11Texture2D,
    },
    Dxgi::Common::*,
};

use crate::{
    AtlasKey, AtlasTextureId, AtlasTextureKind, AtlasTile, Bounds, DevicePixels, PlatformAtlas,
    Point, Size, WindowsAtlasDiagnostics, platform::AtlasTextureList,
};

pub(crate) struct DirectXAtlas(Mutex<DirectXAtlasState>);

struct DirectXAtlasState {
    device: ID3D11Device,
    device_context: ID3D11DeviceContext,
    monochrome_textures: AtlasTextureList<DirectXAtlasTexture>,
    polychrome_textures: AtlasTextureList<DirectXAtlasTexture>,
    tiles_by_key: FxHashMap<AtlasKey, AtlasTile>,
    diagnostics: WindowsAtlasDiagnostics,
    empty_cache: EmptyAtlasCache,
}

const EMPTY_ATLAS_BUDGET_PER_KIND: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
struct EmptyAtlasTexture {
    id: AtlasTextureId,
    width: u32,
    height: u32,
}
impl EmptyAtlasTexture {
    fn payload_bytes(self) -> Option<u64> {
        let channels = match self.id.kind { AtlasTextureKind::Monochrome => 1, AtlasTextureKind::Polychrome => 4 };
        u64::from(self.width).checked_mul(u64::from(self.height))?.checked_mul(channels)
    }
}

#[derive(Default)]
struct EmptyAtlasCache {
    entries: [Option<EmptyAtlasTexture>; 2],
}
impl EmptyAtlasCache {
    fn slot(&mut self, kind: AtlasTextureKind) -> &mut Option<EmptyAtlasTexture> {
        &mut self.entries[match kind { AtlasTextureKind::Monochrome => 0, AtlasTextureKind::Polychrome => 1 }]
    }
    /// Returns the texture to discard and whether it was an older cached entry.
    fn remember(&mut self, candidate: EmptyAtlasTexture) -> (Option<AtlasTextureId>, bool) {
        if candidate.width == 0 || candidate.height == 0 || candidate.payload_bytes()
            .is_none_or(|bytes| bytes > EMPTY_ATLAS_BUDGET_PER_KIND) {
            return (Some(candidate.id), false);
        }
        let slot = self.slot(candidate.id.kind);
        if let Some(old) = slot {
            if old.id == candidate.id { return (None, false); }
            // A larger covering texture serves both geometries. Incomparable
            // aspect ratios favor the most recent workload, within the same cap.
            if old.width >= candidate.width && old.height >= candidate.height {
                return (Some(candidate.id), false);
            }
        }
        let discarded = slot.replace(candidate).map(|old| old.id);
        (discarded, discarded.is_some())
    }
    fn consume(&mut self, id: AtlasTextureId) -> bool {
        let slot = self.slot(id.kind);
        if slot.is_some_and(|entry| entry.id == id) {
            *slot = None;
            true
        } else { false }
    }
    fn gauges(&self) -> (u64, u64) {
        self.entries.iter().flatten().fold((0, 0), |(count, bytes), entry| {
            (count + 1, bytes + entry.payload_bytes().unwrap_or(0))
        })
    }
}

struct DirectXAtlasTexture {
    id: AtlasTextureId,
    bytes_per_pixel: u32,
    allocator: BucketedAtlasAllocator,
    texture: ID3D11Texture2D,
    view: [Option<ID3D11ShaderResourceView>; 1],
    live_atlas_keys: u32,
}

impl DirectXAtlas {
    pub(crate) fn new(device: &ID3D11Device, device_context: &ID3D11DeviceContext) -> Self {
        DirectXAtlas(Mutex::new(DirectXAtlasState {
            device: device.clone(),
            device_context: device_context.clone(),
            monochrome_textures: Default::default(),
            polychrome_textures: Default::default(),
            tiles_by_key: Default::default(),
            diagnostics: Default::default(),
            empty_cache: Default::default(),
        }))
    }

    pub(crate) fn get_texture_view(
        &self,
        id: AtlasTextureId,
    ) -> [Option<ID3D11ShaderResourceView>; 1] {
        let lock = self.0.lock();
        let tex = lock.texture(id);
        tex.view.clone()
    }

    pub(crate) fn diagnostics(&self) -> WindowsAtlasDiagnostics {
        let lock = self.0.lock();
        let mut diagnostics = lock.diagnostics;
        (diagnostics.cached_empty_textures, diagnostics.cached_empty_bytes) = lock.empty_cache.gauges();
        diagnostics
    }

    pub(crate) fn handle_device_lost(
        &self,
        device: &ID3D11Device,
        device_context: &ID3D11DeviceContext,
    ) {
        let mut lock = self.0.lock();
        lock.device = device.clone();
        lock.device_context = device_context.clone();
        lock.monochrome_textures = AtlasTextureList::default();
        lock.polychrome_textures = AtlasTextureList::default();
        lock.tiles_by_key.clear();
        lock.diagnostics.device_resets = lock.diagnostics.device_resets.saturating_add(1);
        lock.diagnostics.live_textures = 0;
        lock.empty_cache = Default::default();
    }
}

impl PlatformAtlas for DirectXAtlas {
    fn get_or_insert_with<'a>(
        &self,
        key: &AtlasKey,
        build: &mut dyn FnMut() -> anyhow::Result<
            Option<(Size<DevicePixels>, std::borrow::Cow<'a, [u8]>)>,
        >,
    ) -> anyhow::Result<Option<AtlasTile>> {
        let mut lock = self.0.lock();
        if let Some(tile) = lock.tiles_by_key.get(key) {
            Ok(Some(tile.clone()))
        } else {
            let Some((size, bytes)) = build()? else {
                return Ok(None);
            };
            let tile = lock
                .allocate(size, key.texture_kind())
                .ok_or_else(|| anyhow::anyhow!("failed to allocate"))?;
            let texture = lock.texture(tile.texture_id);
            texture.upload(&lock.device_context, tile.bounds, &bytes);
            lock.diagnostics.texture_uploads = lock.diagnostics.texture_uploads.saturating_add(1);
            lock.diagnostics.upload_bytes = lock.diagnostics.upload_bytes.saturating_add(bytes.len() as u64);
            lock.tiles_by_key.insert(key.clone(), tile.clone());
            Ok(Some(tile))
        }
    }

    fn remove(&self, key: &AtlasKey) {
        let mut lock = self.0.lock();

        let Some(tile) = lock.tiles_by_key.remove(key) else {
            return;
        };
        lock.diagnostics.tile_removals = lock.diagnostics.tile_removals.saturating_add(1);
        lock.remove_tile(tile);
    }
}

impl DirectXAtlasState {
    fn remove_tile(&mut self, tile: AtlasTile) {
        let id = tile.texture_id;
        let candidate = {
            let textures = match id.kind {
                AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
                AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
            };
            let Some(texture) = textures.textures.get_mut(id.index as usize).and_then(Option::as_mut) else { return; };
            // Releasing every item permits bucket/shelf reclamation even while
            // other keys keep this texture active.
            texture.allocator.deallocate(tile.tile_id.into());
            texture.decrement_ref_count();
            if !texture.is_unreferenced() { return; }
            let size = texture.allocator.size();
            EmptyAtlasTexture { id, width: size.width as u32, height: size.height as u32 }
        };
        let (discarded, evicted_cached) = self.empty_cache.remember(candidate);
        if evicted_cached {
            self.diagnostics.empty_cache_evictions = self.diagnostics.empty_cache_evictions.saturating_add(1);
        }
        if let Some(discarded) = discarded {
            let textures = match discarded.kind {
                AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
                AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
            };
            // Cached slots remain Some and never enter the free list. Only a
            // discarded resource releases its index, once, through this path.
            if textures.textures.get_mut(discarded.index as usize).and_then(Option::take).is_some() {
                textures.free_list.push(discarded.index as usize);
                self.diagnostics.live_textures = self.diagnostics.live_textures.saturating_sub(1);
            }
        }
    }

    fn allocate(
        &mut self,
        size: Size<DevicePixels>,
        texture_kind: AtlasTextureKind,
    ) -> Option<AtlasTile> {
        // Reject impossible requests before creating an unreferenced texture
        // that could never reach normal key-removal/cache retirement.
        if size.width.0 <= 0 || size.height.0 <= 0 || size.width.0 > 16384 || size.height.0 > 16384 {
            return None;
        }
        let reused = {
            let textures = match texture_kind {
                AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
                AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
            };

            textures
                .iter_mut()
                .rev()
                .find_map(|texture| texture.allocate(size))
        };
        if let Some(tile) = reused {
            if self.empty_cache.consume(tile.texture_id) {
                self.diagnostics.empty_cache_reuses = self.diagnostics.empty_cache_reuses.saturating_add(1);
            }
            return Some(tile);
        }

        let texture = self.push_texture(size, texture_kind)?;
        texture.allocate(size)
    }

    fn push_texture(
        &mut self,
        min_size: Size<DevicePixels>,
        kind: AtlasTextureKind,
    ) -> Option<&mut DirectXAtlasTexture> {
        const DEFAULT_ATLAS_SIZE: Size<DevicePixels> = Size {
            width: DevicePixels(1024),
            height: DevicePixels(1024),
        };
        // Max texture size for DirectX. See:
        // https://learn.microsoft.com/en-us/windows/win32/direct3d11/overviews-direct3d-11-resources-limits
        const MAX_ATLAS_SIZE: Size<DevicePixels> = Size {
            width: DevicePixels(16384),
            height: DevicePixels(16384),
        };
        let size = min_size.min(&MAX_ATLAS_SIZE).max(&DEFAULT_ATLAS_SIZE);
        let pixel_format;
        let bind_flag;
        let bytes_per_pixel;
        match kind {
            AtlasTextureKind::Monochrome => {
                pixel_format = DXGI_FORMAT_R8_UNORM;
                bind_flag = D3D11_BIND_SHADER_RESOURCE;
                bytes_per_pixel = 1;
            }
            AtlasTextureKind::Polychrome => {
                pixel_format = DXGI_FORMAT_B8G8R8A8_UNORM;
                bind_flag = D3D11_BIND_SHADER_RESOURCE;
                bytes_per_pixel = 4;
            }
        }
        let texture_desc = D3D11_TEXTURE2D_DESC {
            Width: size.width.0 as u32,
            Height: size.height.0 as u32,
            MipLevels: 1,
            ArraySize: 1,
            Format: pixel_format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind_flag.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        unsafe {
            // This only returns None if the device is lost, which we will recreate later.
            // So it's ok to return None here.
            self.device
                .CreateTexture2D(&texture_desc, None, Some(&mut texture))
                .ok()?;
        }
        let texture = texture.unwrap();
        self.diagnostics.texture_creates = self.diagnostics.texture_creates.saturating_add(1);

        let view = unsafe {
            let mut view = None;
            self.device
                .CreateShaderResourceView(&texture, None, Some(&mut view))
                .ok()?;
            [view]
        };
        // A failed SRV creation must not consume a reusable empty index.
        let texture_list = match kind {
            AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
            AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
        };
        let index = texture_list.free_list.pop();
        let atlas_texture = DirectXAtlasTexture {
            id: AtlasTextureId {
                index: index.unwrap_or(texture_list.textures.len()) as u32,
                kind,
            },
            bytes_per_pixel,
            allocator: etagere::BucketedAtlasAllocator::new(size.into()),
            texture,
            view,
            live_atlas_keys: 0,
        };
        self.diagnostics.live_textures = self.diagnostics.live_textures.saturating_add(1);
        self.diagnostics.peak_live_textures = self.diagnostics.peak_live_textures.max(self.diagnostics.live_textures);
        if let Some(ix) = index {
            texture_list.textures[ix] = Some(atlas_texture);
            texture_list.textures.get_mut(ix).unwrap().as_mut()
        } else {
            texture_list.textures.push(Some(atlas_texture));
            texture_list.textures.last_mut().unwrap().as_mut()
        }
    }

    fn texture(&self, id: AtlasTextureId) -> &DirectXAtlasTexture {
        let textures = match id.kind {
            crate::AtlasTextureKind::Monochrome => &self.monochrome_textures,
            crate::AtlasTextureKind::Polychrome => &self.polychrome_textures,
        };
        textures[id.index as usize].as_ref().unwrap()
    }
}

impl DirectXAtlasTexture {
    fn allocate(&mut self, size: Size<DevicePixels>) -> Option<AtlasTile> {
        let allocation = self.allocator.allocate(size.into())?;
        let tile = AtlasTile {
            texture_id: self.id,
            tile_id: allocation.id.into(),
            bounds: Bounds {
                origin: allocation.rectangle.min.into(),
                size,
            },
            padding: 0,
        };
        self.live_atlas_keys += 1;
        Some(tile)
    }

    fn upload(
        &self,
        device_context: &ID3D11DeviceContext,
        bounds: Bounds<DevicePixels>,
        bytes: &[u8],
    ) {
        unsafe {
            device_context.UpdateSubresource(
                &self.texture,
                0,
                Some(&D3D11_BOX {
                    left: bounds.left().0 as u32,
                    top: bounds.top().0 as u32,
                    front: 0,
                    right: bounds.right().0 as u32,
                    bottom: bounds.bottom().0 as u32,
                    back: 1,
                }),
                bytes.as_ptr() as _,
                bounds.size.width.to_bytes(self.bytes_per_pixel as u8),
                0,
            );
        }
    }

    fn decrement_ref_count(&mut self) {
        self.live_atlas_keys -= 1;
    }

    fn is_unreferenced(&mut self) -> bool {
        self.live_atlas_keys == 0
    }
}

impl From<Size<DevicePixels>> for etagere::Size {
    fn from(size: Size<DevicePixels>) -> Self {
        etagere::Size::new(size.width.into(), size.height.into())
    }
}

impl From<etagere::Point> for Point<DevicePixels> {
    fn from(value: etagere::Point) -> Self {
        Point {
            x: DevicePixels::from(value.x),
            y: DevicePixels::from(value.y),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn empty(index: u32, kind: AtlasTextureKind, width: u32, height: u32) -> EmptyAtlasTexture {
        EmptyAtlasTexture { id: AtlasTextureId { index, kind }, width, height }
    }

    #[test]
    fn empty_video_texture_is_reused_repeatedly_without_growing_cache() {
        let mut cache = EmptyAtlasCache::default();
        let video = empty(7, AtlasTextureKind::Polychrome, 1280, 1024);
        for _ in 0..300 {
            assert_eq!(cache.remember(video), (None, false));
            assert_eq!(cache.gauges(), (1, 5 * 1024 * 1024));
            assert!(!cache.consume(AtlasTextureId { index: 8, ..video.id }));
            assert!(cache.consume(video.id));
            assert_eq!(cache.gauges(), (0, 0));
            assert!(!cache.consume(video.id), "A tag can only be consumed once");
        }
    }

    #[test]
    fn cache_preserves_covering_capacity_and_replaces_incomparable_geometry() {
        let mut cache = EmptyAtlasCache::default();
        let large = empty(1, AtlasTextureKind::Polychrome, 2048, 2048);
        assert_eq!(cache.remember(large), (None, false));
        let small = empty(2, AtlasTextureKind::Polychrome, 1280, 1024);
        assert_eq!(cache.remember(small), (Some(small.id), false));
        assert!(cache.consume(large.id));
        let landscape = empty(3, AtlasTextureKind::Polychrome, 1280, 1024);
        assert_eq!(cache.remember(landscape), (None, false));
        let portrait = empty(4, AtlasTextureKind::Polychrome, 1024, 1280);
        assert_eq!(cache.remember(portrait), (Some(landscape.id), true));
        assert!(!cache.consume(landscape.id));
        assert_eq!(cache.gauges(), (1, 5 * 1024 * 1024));
        assert!(cache.consume(portrait.id));
    }

    #[test]
    fn cache_budget_is_per_kind_and_rejects_oversize_zero_and_overflow() {
        let mut cache = EmptyAtlasCache::default();
        let color = empty(1, AtlasTextureKind::Polychrome, 2048, 2048);
        let mono = empty(1, AtlasTextureKind::Monochrome, 4096, 4096);
        assert_eq!(cache.remember(color), (None, false));
        assert_eq!(cache.remember(mono), (None, false));
        assert_eq!(cache.gauges(), (2, 32 * 1024 * 1024));
        for (width, height) in [(2048, 2049), (0, 1024), (u32::MAX, u32::MAX)] {
            let oversized = empty(9, AtlasTextureKind::Polychrome, width, height);
            assert_eq!(cache.remember(oversized), (Some(oversized.id), false));
            assert_eq!(cache.gauges(), (2, 32 * 1024 * 1024));
        }
        assert!(cache.consume(color.id));
        assert!(cache.consume(mono.id));
        assert_eq!(cache.gauges(), (0, 0));
    }

    #[test]
    fn cache_reset_forgets_old_texture_ids_before_their_indices_can_be_reused() {
        let mut cache = EmptyAtlasCache::default();
        let old = empty(0, AtlasTextureKind::Polychrome, 1280, 1024);
        cache.remember(old);
        cache = Default::default();
        assert_eq!(cache.gauges(), (0, 0));
        assert!(!cache.consume(old.id));
        let replacement = empty(0, AtlasTextureKind::Polychrome, 1024, 1280);
        assert_eq!(cache.remember(replacement), (None, false));
        assert_eq!(cache.gauges(), (1, 5 * 1024 * 1024));
    }
}
