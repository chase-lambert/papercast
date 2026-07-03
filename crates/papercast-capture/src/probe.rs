//! Runtime inspection of the Wayland compositor: which globals exist, what
//! outputs look like, which shm formats are offered, and — the question that
//! decides our capture backend — which screen-capture protocol families are
//! actually present.

use wayland_client::{
    protocol::{wl_output, wl_registry, wl_shm},
    Connection, Dispatch, QueueHandle, WEnum,
};

/// One entry from the compositor's global registry.
#[derive(Debug, Clone)]
pub struct GlobalEntry {
    pub interface: String,
    pub version: u32,
}

/// What we learned about one monitor.
#[derive(Debug, Clone, Default)]
pub struct OutputInfo {
    pub name: Option<String>,
    pub description: Option<String>,
    /// (width, height, refresh_mHz) of the current mode.
    pub mode: Option<(i32, i32, i32)>,
    pub scale: i32,
    pub transform: Option<String>,
}

/// A capture protocol family we know how to (eventually) drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureTier {
    /// ext-image-copy-capture-v1 + ext-image-capture-source-v1 (standard).
    ExtImageCopyCapture,
    /// COSMIC-specific zcosmic_* screencopy protocols.
    CosmicScreencopy,
    /// Legacy wlroots zwlr_screencopy_manager_v1.
    WlrScreencopy,
}

#[derive(Debug, Default)]
pub struct ProbeReport {
    pub globals: Vec<GlobalEntry>,
    pub outputs: Vec<OutputInfo>,
    pub shm_formats: Vec<String>,
}

impl ProbeReport {
    pub fn global_version(&self, interface: &str) -> Option<u32> {
        self.globals
            .iter()
            .find(|g| g.interface == interface)
            .map(|g| g.version)
    }

    /// All globals whose names suggest screen-capture machinery, including
    /// families we don't know — so an unexpected compositor still tells us
    /// what it *does* offer instead of silently reporting "nothing".
    pub fn capture_like_globals(&self) -> Vec<&GlobalEntry> {
        const HINTS: &[&str] = &["screencopy", "image_copy_capture", "image_capture_source", "image_source", "screencast", "export_dmabuf"];
        self.globals
            .iter()
            .filter(|g| HINTS.iter().any(|h| g.interface.contains(h)))
            .collect()
    }

    /// Best supported capture tier, in our priority order.
    pub fn best_tier(&self) -> Option<CaptureTier> {
        self.supported_tiers().into_iter().next()
    }

    pub fn supported_tiers(&self) -> Vec<CaptureTier> {
        let mut tiers = Vec::new();
        // The standard protocol splits "what to capture" (source manager)
        // from "how to copy it" (capture manager); we need both.
        if self.global_version("ext_image_copy_capture_manager_v1").is_some()
            && self
                .global_version("ext_output_image_capture_source_manager_v1")
                .is_some()
        {
            tiers.push(CaptureTier::ExtImageCopyCapture);
        }
        if self.globals.iter().any(|g| {
            g.interface.starts_with("zcosmic_screencopy_manager_v")
        }) {
            tiers.push(CaptureTier::CosmicScreencopy);
        }
        if self.global_version("zwlr_screencopy_manager_v1").is_some() {
            tiers.push(CaptureTier::WlrScreencopy);
        }
        tiers
    }
}

/// State struct the Wayland event queue dispatches into. Each `Dispatch`
/// impl below is "when an event for this interface arrives, mutate me".
#[derive(Default)]
struct ProbeState {
    report: ProbeReport,
}

impl Dispatch<wl_registry::WlRegistry, ()> for ProbeState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            // Bind outputs/shm as we discover them so their detail events
            // arrive on the next roundtrip.
            match interface.as_str() {
                "wl_output" => {
                    let idx = state.report.outputs.len();
                    state.report.outputs.push(OutputInfo::default());
                    // v4 adds the `name`/`description` events; ask for at
                    // most what the compositor advertises.
                    registry.bind::<wl_output::WlOutput, usize, Self>(
                        name,
                        version.min(4),
                        qh,
                        idx,
                    );
                }
                "wl_shm" => {
                    registry.bind::<wl_shm::WlShm, (), Self>(name, 1, qh, ());
                }
                _ => {}
            }
            state.report.globals.push(GlobalEntry { interface, version });
        }
    }
}

impl Dispatch<wl_output::WlOutput, usize> for ProbeState {
    fn event(
        state: &mut Self,
        _output: &wl_output::WlOutput,
        event: wl_output::Event,
        &idx: &usize,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let Some(info) = state.report.outputs.get_mut(idx) else { return };
        match event {
            wl_output::Event::Name { name } => info.name = Some(name),
            wl_output::Event::Description { description } => {
                info.description = Some(description)
            }
            wl_output::Event::Mode { flags, width, height, refresh } => {
                if matches!(flags, WEnum::Value(f) if f.contains(wl_output::Mode::Current)) {
                    info.mode = Some((width, height, refresh));
                }
            }
            wl_output::Event::Scale { factor } => info.scale = factor,
            wl_output::Event::Geometry { transform, .. } => {
                info.transform = match transform {
                    WEnum::Value(t) => Some(format!("{t:?}")),
                    WEnum::Unknown(v) => Some(format!("unknown({v})")),
                };
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_shm::WlShm, ()> for ProbeState {
    fn event(
        state: &mut Self,
        _shm: &wl_shm::WlShm,
        event: wl_shm::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_shm::Event::Format { format } = event {
            let s = match format {
                WEnum::Value(f) => format!("{f:?}"),
                WEnum::Unknown(v) => format!("unknown(0x{v:08x})"),
            };
            state.report.shm_formats.push(s);
        }
    }
}

/// Connect to the compositor named by `$WAYLAND_DISPLAY` and gather the report.
pub fn run() -> anyhow::Result<ProbeReport> {
    let conn = Connection::connect_to_env()?;
    let display = conn.display();
    let mut queue = conn.new_event_queue::<ProbeState>();
    let qh = queue.handle();
    let _registry = display.get_registry(&qh, ());

    let mut state = ProbeState::default();
    // First roundtrip: registry globals arrive (and we bind output/shm).
    // Second: the bound objects' own events (modes, names, formats) arrive.
    queue.roundtrip(&mut state)?;
    queue.roundtrip(&mut state)?;

    state.report.globals.sort_by(|a, b| a.interface.cmp(&b.interface));
    Ok(state.report)
}
