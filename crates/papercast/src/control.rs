//! Control socket: a Unix-domain-socket + newline-delimited-JSON protocol that
//! lets `papercast ctl ...` change the running mirror's display mode, force a
//! full refresh, or query status. Wayland has no global-hotkey API, so the
//! intended use is binding compositor shortcuts to `papercast ctl` commands.
//!
//! The `run` side ([`spawn_server`]) owns the shared [`ModeState`]; the `ctl`
//! side ([`send`]) is a plain blocking client (like `probe`, no tokio).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context};
use papercast_core::dither::DitherMode;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as TokioBufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

use crate::mode::{ModeSettings, ModeState};

/// A control request. Serialized as `{"cmd":"mode","name":"writing"}` etc.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum Request {
    /// Switch the active display mode.
    Mode { name: String },
    /// Force one full-frame redraw now (the "clear ghosts" button).
    Refresh,
    /// Report the effective settings.
    Status,
}

/// A control response: `{"ok":true,...}` or `{"ok":false,"error":"..."}`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
}

/// The effective runtime state, for `ctl status`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Status {
    /// Active mode name, or `None` for plain base config.
    pub mode: Option<String>,
    pub fps: u32,
    pub levels: u8,
    pub dither: DitherMode,
    pub tile_size: u32,
    pub full_refresh_secs: u64,
    pub full_refresh_updates: u64,
    /// Output framebuffer size (width, height).
    pub framebuffer: (u32, u32),
    /// Captured output name, if one was selected.
    pub output: Option<String>,
}

/// The control socket path: `$XDG_RUNTIME_DIR/papercast.sock`, else
/// `/tmp/papercast-$UID.sock`.
pub fn socket_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(dir).join("papercast.sock")
    } else {
        let uid = rustix::process::getuid().as_raw();
        PathBuf::from(format!("/tmp/papercast-{uid}.sock"))
    }
}

/// Removes the socket file when `run` exits (best-effort; a hard kill is
/// covered by the stale-socket check on the next startup).
pub struct SocketGuard(PathBuf);
impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Everything the server needs to service a request.
pub struct ServerCtx {
    pub state: Arc<Mutex<ModeState>>,
    pub settings_tx: watch::Sender<ModeSettings>,
    pub fps_tx: watch::Sender<u32>,
    pub refresh_tx: mpsc::Sender<()>,
    pub framebuffer: (u32, u32),
    pub output: Option<String>,
}

/// Bind the control socket and spawn the accept loop. Hardening: if the socket
/// file already exists, try to connect — a live peer means another papercast is
/// running (error out); a refused/failed connect means it's stale (unlink and
/// rebind). The returned guard removes the file on clean exit.
pub fn spawn_server(ctx: ServerCtx) -> anyhow::Result<SocketGuard> {
    let path = socket_path();

    if path.exists() {
        match StdUnixStream::connect(&path) {
            Ok(_) => bail!(
                "another papercast is already running (control socket {} is live)",
                path.display()
            ),
            Err(_) => {
                warn!("removing stale control socket {}", path.display());
                std::fs::remove_file(&path)
                    .with_context(|| format!("removing stale socket {}", path.display()))?;
            }
        }
    }

    let listener = UnixListener::bind(&path)
        .with_context(|| format!("binding control socket {}", path.display()))?;
    // User-only: the session is unauthenticated; no other user should drive it.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {}", path.display()))?;
    info!("control socket at {}", path.display());

    let ctx = Arc::new(ctx);
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let ctx = Arc::clone(&ctx);
                    tokio::spawn(async move {
                        if let Err(e) = handle_conn(stream, &ctx).await {
                            warn!("control connection error: {e:#}");
                        }
                    });
                }
                Err(e) => {
                    error!("control socket accept failed: {e}");
                    return;
                }
            }
        }
    });

    Ok(SocketGuard(path))
}

/// Read newline-delimited requests from one connection and answer each.
async fn handle_conn(stream: UnixStream, ctx: &ServerCtx) -> anyhow::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = TokioBufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => dispatch(req, ctx).await,
            Err(e) => Response { ok: false, error: Some(format!("bad request: {e}")), status: None },
        };
        let mut buf = serde_json::to_vec(&response)?;
        buf.push(b'\n');
        write.write_all(&buf).await?;
    }
    Ok(())
}

/// Apply one request. Mode/refresh mutate the running mirror.
async fn dispatch(req: Request, ctx: &ServerCtx) -> Response {
    match req {
        Request::Status => match build_status(ctx) {
            Ok(status) => Response { ok: true, error: None, status: Some(status) },
            Err(e) => Response { ok: false, error: Some(e), status: None },
        },
        Request::Refresh => {
            // Best-effort: the serve loop coalesces refreshes anyway.
            let _ = ctx.refresh_tx.send(()).await;
            Response { ok: true, error: None, status: None }
        }
        Request::Mode { name } => match apply_mode(ctx, &name) {
            Ok(status) => Response { ok: true, error: None, status: Some(status) },
            Err(e) => Response { ok: false, error: Some(e), status: None },
        },
    }
}

/// Switch mode and broadcast the new effective settings — all under the state
/// lock, so a config save racing this switch can't interleave and leave a watch
/// channel carrying stale settings. `watch::Sender::send` is synchronous (no
/// await), so holding the std `Mutex` across it is safe. The serve loop reacts
/// (full redraw); the source re-paces off `fps_tx`.
fn apply_mode(ctx: &ServerCtx, name: &str) -> Result<Status, String> {
    let (mode, effective) = {
        let mut state = ctx.state.lock().map_err(|_| "mode state poisoned".to_string())?;
        let old = state.active().map(str::to_string);
        state.set_mode(name).map_err(|e| e.to_string())?;
        info!("mode {} -> {name} (via ctl)", old.as_deref().unwrap_or("none"));
        let effective = state.effective();
        // Send fps first so the source is already re-paced by the time the
        // serve loop redraws.
        let _ = ctx.fps_tx.send(effective.fps);
        let _ = ctx.settings_tx.send(effective.clone());
        (state.active().map(str::to_string), effective)
    };
    Ok(status_from(ctx, mode, &effective))
}

fn build_status(ctx: &ServerCtx) -> Result<Status, String> {
    let state = ctx.state.lock().map_err(|_| "mode state poisoned".to_string())?;
    let mode = state.active().map(str::to_string);
    Ok(status_from(ctx, mode, &state.effective()))
}

fn status_from(ctx: &ServerCtx, mode: Option<String>, e: &ModeSettings) -> Status {
    Status {
        mode,
        fps: e.fps,
        levels: e.eink.levels,
        dither: e.eink.dither,
        tile_size: e.tile_size,
        full_refresh_secs: e.full_refresh_secs,
        full_refresh_updates: e.full_refresh_updates,
        framebuffer: ctx.framebuffer,
        output: ctx.output.clone(),
    }
}

// --- client side (`papercast ctl ...`) ---

/// Send one request to a running `papercast run` and return its response.
pub fn send(req: &Request) -> anyhow::Result<Response> {
    let path = socket_path();
    let stream = StdUnixStream::connect(&path).map_err(|_| {
        anyhow!("cannot reach papercast at {} — is 'papercast run' running?", path.display())
    })?;
    let mut writer = stream.try_clone().context("cloning control stream")?;
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).context("writing control request")?;
    writer.flush().ok();

    let mut resp_line = String::new();
    BufReader::new(stream).read_line(&mut resp_line).context("reading control response")?;
    let resp: Response =
        serde_json::from_str(resp_line.trim()).context("parsing control response")?;
    Ok(resp)
}
