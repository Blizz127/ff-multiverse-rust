//! Test support: draw a scene plus a UI overlay into an offscreen texture the way the window does,
//! read the pixels back and write PNGs to look at. They need a GPU or a software Vulkan/GL driver
//! (lavapipe/llvmpipe on a machine without a display), so the tests that use them are `#[ignore]`d:
//! `cargo test -p fmv-client -- --ignored offscreen`.

use crate::render::{backdrop_quad, build_vertices, Gpu, Scene, CLEAR_RGB};
use crate::ui::draw::DrawList;
use std::path::PathBuf;

/// Where the screenshots go: `$FMV_SHOT_DIR`, else `~/.cache/claude-tmp/fmv-client-shots`.
pub fn shot_dir() -> PathBuf {
    match std::env::var_os("FMV_SHOT_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()))
            .join(".cache/claude-tmp/fmv-client-shots"),
    }
}

/// A GPU context without a window.
pub struct Offscreen {
    device: wgpu::Device,
    queue: wgpu::Queue,
    gpu: Gpu,
}

impl Offscreen {
    /// `None` (with a note on stderr) when no adapter is available.
    pub fn new() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("skipped: no GPU adapter available ({e})");
                return None;
            }
        };
        eprintln!("offscreen adapter: {:?}", adapter.get_info().name);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
            ..wgpu::DeviceDescriptor::default()
        }))
        .expect("device");
        let gpu = Gpu::new(device.clone(), queue.clone(), wgpu::TextureFormat::Rgba8Unorm);
        Some(Self { device, queue, gpu })
    }

    /// Render `scene` and then `overlay` into a `w` x `h` target; tightly packed RGBA8, top row
    /// first. Colours are not sRGB-converted (the target is plain `Rgba8Unorm`).
    pub fn render(&mut self, w: u32, h: u32, scene: &Scene<'_>, overlay: &DrawList) -> Vec<u8> {
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let vertices = build_vertices(scene, w, h, false);
        // The picture behind the walkmesh, if the scene has one, goes to the GPU like in the window.
        let quad = match scene.backdrop {
            Some(b) if self.gpu.ensure_backdrop(b) => backdrop_quad(scene, w, h),
            _ => None,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.gpu.draw_scene(&view, &vertices, quad.as_ref(), CLEAR_RGB, false);
        self.gpu.draw_overlay(&view, &overlay.vertices(w, h, false));

        // Rows of a texture-to-buffer copy are padded to 256 bytes.
        let padded = (w * 4).next_multiple_of(256);
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(padded * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit(std::iter::once(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        readback.slice(..).map_async(wgpu::MapMode::Read, move |r| tx.send(r).expect("send"));
        self.device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        rx.recv().expect("map callback").expect("map");
        let data = readback.slice(..).get_mapped_range().expect("mapped range");
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in data.chunks(padded as usize).take(h as usize) {
            out.extend_from_slice(&row[..(w * 4) as usize]);
        }
        out
    }
}

// ---------------------------------------------------------------------------------------------
// A fake server on a real socket
// ---------------------------------------------------------------------------------------------

/// How a [`serve_join`] server answers the `Hello`.
pub enum Reply {
    /// Welcome (issuing `token` if given), the fixture area, and a snapshot whose own entity is
    /// `stored` (the profile a server would hold for a login); without it, the look the `Hello`
    /// asked for (a registration). With a `gate`, the server waits for a message on it before it
    /// sends the `Welcome`: a test can change something on disk and then release it, with no
    /// timing involved.
    Join { stored: Option<fmv_core::AvatarAppearance>, token: Option<&'static str>, gate: Option<std::sync::mpsc::Receiver<()>> },
    /// Refuse the `Hello` with this code and sentence (the client acts on the code).
    Reject(fmv_protocol::RejectCode, &'static str),
}

/// What a [`serve_join`] server saw.
#[derive(Debug)]
pub struct Seen {
    pub hello: fmv_protocol::ClientMsg,
    pub join: Option<fmv_protocol::ClientMsg>,
    /// The client said goodbye (`Leave`).
    pub left: bool,
}

/// The first spawn point of the fixture's area.
fn spawn_of(adapter: &fmv_fixture::FixtureAdapter) -> fmv_core::Pos {
    use fmv_adapter_api::GameAdapter;
    let id = adapter.list_areas().remove(0).id;
    adapter.load_area(&id.key).ok().and_then(|a| a.spawn_points().first().copied()).unwrap_or_default()
}

/// A fake server for ONE connection on a local port, answering the handshake like the real one.
/// Returns its address and a handle for what it saw (the thread ends after `Leave`, when the
/// client hangs up, or after 30 s).
pub fn serve_join(reply: Reply) -> (String, std::thread::JoinHandle<Seen>) {
    use fmv_adapter_api::GameAdapter;
    use fmv_core::{AccountId, AnimState, AvatarAppearance, EntityId, EntitySnapshot, SessionId};
    use fmv_fixture::FixtureAdapter;
    use fmv_protocol::{read_msg, write_msg, Auth, ClientMsg, Secret, ServerMsg, PROTOCOL_VERSION};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        rt.block_on(async move {
            let limit = std::time::Duration::from_secs(30);
            listener.set_nonblocking(true).expect("nonblocking");
            let listener = tokio::net::TcpListener::from_std(listener).expect("listener");
            let (stream, _) = tokio::time::timeout(limit, listener.accept()).await.expect("a client in time").expect("accept");
            let (mut rd, mut wr) = stream.into_split();
            let hello: ClientMsg = tokio::time::timeout(limit, read_msg(&mut rd)).await.expect("a Hello in time").expect("Hello");
            let mut seen = Seen { hello: hello.clone(), join: None, left: false };
            let (stored, token) = match reply {
                Reply::Reject(code, reason) => {
                    write_msg(&mut wr, &ServerMsg::reject(code, reason)).await.expect("reject");
                    return seen;
                }
                Reply::Join { stored, token, gate } => {
                    if let Some(gate) = gate {
                        // Blocks this server thread only (it serves one connection, nothing else
                        // needs to run meanwhile). A dropped sender releases it too.
                        let _ = gate.recv_timeout(limit);
                    }
                    (stored, token)
                }
            };
            let ClientMsg::Hello { auth, tint, appearance_ref, .. } = hello else { panic!("the first message must be Hello") };
            let own = stored.unwrap_or_else(|| match auth {
                Auth::Register { display_name } => AvatarAppearance::new(display_name, tint, appearance_ref),
                Auth::Token(_) => AvatarAppearance::new("tester", tint, appearance_ref),
            });
            let adapter = FixtureAdapter::new();
            let info = adapter.list_areas().remove(0);
            let welcome = ServerMsg::Welcome {
                protocol_version: PROTOCOL_VERSION,
                server_build: "fake-server/1".into(),
                session: SessionId(1),
                entity: EntityId(7),
                account: AccountId(42),
                issued_token: token.map(Secret::new),
            };
            write_msg(&mut wr, &welcome).await.expect("welcome");
            let manifest = ServerMsg::AreaManifest {
                area: info.id.clone(),
                display_name: info.display_name.clone(),
                fingerprint: info.fingerprint,
                provenance: info.provenance.summary(),
                fixture: info.fixture,
            };
            write_msg(&mut wr, &manifest).await.expect("manifest");
            seen.join = Some(tokio::time::timeout(limit, read_msg(&mut rd)).await.expect("JoinArea in time").expect("JoinArea"));
            let me = EntitySnapshot { id: EntityId(7), pos: spawn_of(&adapter), facing: 0.0, anim: AnimState::Idle, avatar: own };
            write_msg(&mut wr, &ServerMsg::AreaSnapshot { area: info.id, tick: 1, entities: vec![me] }).await.expect("snapshot");
            loop {
                match tokio::time::timeout(limit, read_msg::<_, ClientMsg>(&mut rd)).await {
                    Ok(Ok(ClientMsg::Leave)) => {
                        seen.left = true;
                        return seen;
                    }
                    Ok(Ok(ClientMsg::Ping { nonce })) => {
                        let _ = write_msg(&mut wr, &ServerMsg::Pong { nonce, tick: 2 }).await;
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(_)) | Err(_) => return seen,
                }
            }
        })
    });
    (addr, handle)
}

/// A tiny deterministic random generator (xorshift64*), for the monkey tests: the same seed is
/// the same run, so a failure can be reproduced.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number in `0..n` (`n` of 0 gives 0).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    pub fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }

    /// A character from a mix a keyboard (or a hostile server) could produce: letters, digits,
    /// spaces, punctuation, accents, CJK, emoji, control and invisible characters.
    pub fn character(&mut self) -> char {
        const POOL: [char; 24] = [
            'a', 'Z', 'q', '0', '9', ' ', ' ', '-', '_', '.', '#', '~', '\u{e9}', '\u{4e2d}', '\u{1f600}', '\u{200b}',
            '\u{202e}', '\n', '\t', '\u{1b}', '\u{7f}', 'W', 'i', '!',
        ];
        POOL[self.below(POOL.len())]
    }

    pub fn string(&mut self, max: usize) -> String {
        let n = self.below(max + 1);
        (0..n).map(|_| self.character()).collect()
    }
}

// ---------------------------------------------------------------------------------------------
// Monkey tests of the menu
// ---------------------------------------------------------------------------------------------

use crate::ui::menu::{CharacterEntry, KeyPress, Look, Menu, MenuConfig, NavKey, Notice, Screen, SelectState};
use fmv_core::GameId;

fn random_entries(rng: &mut Rng, max: usize) -> Vec<CharacterEntry> {
    (0..rng.below(max + 1))
        .map(|i| CharacterEntry {
            name: format!("Hero {i}{}", rng.below(3)),
            look: rng.chance(2).then(|| Look { tint: [rng.below(256) as u8, 1, 2], appearance: None }),
            loose_permissions: rng.chance(4),
        })
        .collect()
}

/// What must hold of a menu whatever was pressed.
pub fn assert_sane(m: &Menu, what: &str) {
    let list_ok = |s: &SelectState| {
        assert!(!m.accounts().is_empty(), "{what}: a list with nothing in it");
        assert!(s.cursor < m.accounts().len(), "{what}: cursor {} past {}", s.cursor, m.accounts().len());
        if let Some(c) = s.confirm {
            assert!(c.index < m.accounts().len(), "{what}: confirmation about a missing character");
        }
    };
    match m.screen() {
        Screen::Select(s) => list_ok(s),
        Screen::Create(f) => {
            assert!(f.name.len() <= 24, "{what}: {} characters in the name", f.name.len());
            assert!(f.palette_cursor < crate::ui::palette::PALETTE.len(), "{what}");
            assert!(f.fields().contains(&f.focus), "{what}: focus on a field that is not there");
            if f.has_looks() {
                assert!(f.appearance < m.appearances().len(), "{what}");
            }
            assert!(f.name.cursor() <= f.name.len(), "{what}");
        }
        // The screen an attempt comes back to is made valid when it comes back (`Menu::restored`).
        Screen::Connecting(_) | Screen::Loading(_) => {}
    }
    assert!(m.clock().is_finite(), "{what}");
}

fn random_key(rng: &mut Rng) -> KeyPress {
    use NavKey::*;
    const KEYS: [NavKey; 13] = [Up, Down, Left, Right, Home, End, PageUp, PageDown, Enter, Escape, Tab, Backspace, Delete];
    KeyPress { key: KEYS[rng.below(KEYS.len())], shift: rng.chance(4), repeat: rng.chance(5) }
}

/// Drive a menu with `steps` random events (keys, typed text, list changes, failures, time,
/// messages) and check after each that it is consistent; `each` sees the menu after every step.
/// Never panics on any input.
pub fn monkey(seed: u64, steps: usize, mut each: impl FnMut(&Menu, usize)) {
    let mut rng = Rng::new(seed);
    let game = [GameId::Ff7, GameId::Fixture][rng.below(2)];
    let mut m = Menu::new(MenuConfig::new("127.0.0.1:47770", game), random_entries(&mut rng, 6));
    for step in 0..steps {
        match rng.below(16) {
            0..=7 => {
                let press = random_key(&mut rng);
                m.key(press);
            }
            8..=10 => {
                let text = rng.string(6);
                m.text(&text);
            }
            11 => {
                let list = random_entries(&mut rng, 6);
                m.refresh_accounts(list);
            }
            12 => m.connect_failed(Notice::error(rng.string(80)).with_hint(rng.string(80))),
            13 => m.tick(rng.below(300) as f32 / 100.0 - 0.5),
            14 if rng.chance(6) => m.set_notice(Notice::warning(rng.string(30)).with_secret(rng.string(60))),
            14 => m.set_notice(Notice::info(rng.string(30))),
            _ if rng.chance(8) => m.loading(rng.string(30)),
            _ => {}
        }
        assert_sane(&m, &format!("seed {seed} step {step}"));
        each(&m, step);
    }
}

/// One pixel of a packed RGBA image.
pub fn pixel(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * w + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2]]
}

/// Write `rgba` as `<shot dir>/<name>.png` and return the path.
pub fn save(name: &str, w: u32, h: u32, rgba: &[u8]) -> PathBuf {
    let dir = shot_dir();
    std::fs::create_dir_all(&dir).expect("shot dir");
    let file = dir.join(format!("{name}.png"));
    std::fs::write(&file, write_png(w, h, rgba)).expect("write png");
    eprintln!("wrote {}", file.display());
    file
}

/// Minimal PNG encoder (stored deflate blocks, no compression).
pub fn write_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFF_u32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
            }
        }
        !crc
    }
    fn adler32(data: &[u8]) -> u32 {
        let (mut a, mut b) = (1_u32, 0_u32);
        for &x in data {
            a = (a + u32::from(x)) % 65_521;
            b = (b + a) % 65_521;
        }
        (b << 16) | a
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    let mut raw = Vec::with_capacity((w as usize * 4 + 1) * h as usize);
    for row in rgba.chunks(w as usize * 4) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65_535).peekable();
    while let Some(block) = blocks.next() {
        z.push(u8::from(blocks.peek().is_none()));
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// An empty scene: the renderer clears to its background colour and draws nothing else, which is
/// what the pre-join menus sit on.
pub fn empty_scene() -> Scene<'static> {
    crate::ui::screens::menu_scene()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_png_writer_makes_a_valid_header_and_end_chunk() {
        let png = write_png(2, 1, &[1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 2);
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }
}
