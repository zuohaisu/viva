//! Bounded background socket clients. Git/network projections and large
//! snapshots never sit in the physical terminal's event/render path.
use super::WorkbenchModel;
use crate::foundation::OfficeResult;
use crate::office::{OfficeClient, OfficeRequestKind};
use crate::terminal::TerminalSnapshot;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc::{self, Receiver, SyncSender},
};
pub type Viewport = (String, u16, u16, usize);
pub struct Polling {
    pub models: Receiver<Result<WorkbenchModel, String>>,
    pub screens: Receiver<(String, Result<TerminalSnapshot, String>)>,
    pub errors: Receiver<String>,
    viewports: SyncSender<Vec<Viewport>>,
    input: SyncSender<(String, Vec<u8>)>,
    queued: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}
impl Polling {
    pub fn start(client: &OfficeClient) -> OfficeResult<Self> {
        let mut model_client = client.independent()?;
        let mut screen_client = client.independent()?;
        let mut input_client = client.independent()?;
        let stop = Arc::new(AtomicBool::new(false));
        let queued = Arc::new(AtomicUsize::new(0));
        let (model_tx, models) = mpsc::sync_channel(1);
        let (screen_tx, screens) = mpsc::sync_channel(super::super::layout::MAX_PANES);
        let (viewports, view_rx) = mpsc::sync_channel::<Vec<Viewport>>(1);
        let (input, input_rx) = mpsc::sync_channel::<(String, Vec<u8>)>(32);
        let (error_tx, errors) = mpsc::sync_channel(1);
        let halt = stop.clone();
        std::thread::spawn(move || {
            while !halt.load(Ordering::Relaxed) {
                let result = model_client
                    .call(OfficeRequestKind::WorkbenchView)
                    .and_then(|v| serde_json::from_value(v).map_err(Into::into))
                    .map_err(|e| e.to_string());
                if model_tx.try_send(result).is_err() && halt.load(Ordering::Relaxed) {
                    break;
                }
                for _ in 0..10 {
                    if halt.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        });
        let halt = stop.clone();
        std::thread::spawn(move || {
            let mut sizes = std::collections::HashMap::new();
            while !halt.load(Ordering::Relaxed) {
                let Ok(mut views) = view_rx.recv_timeout(std::time::Duration::from_millis(100))
                else {
                    continue;
                };
                while let Ok(next) = view_rx.try_recv() {
                    views = next;
                }
                sizes.retain(|id, _| views.iter().any(|(v, _, _, _)| v == id));
                for (id, cols, rows, offset) in views {
                    if halt.load(Ordering::Relaxed) {
                        return;
                    }
                    let result = (|| {
                        if sizes.get(&id) != Some(&(cols, rows)) {
                            screen_client.call(OfficeRequestKind::TerminalResize {
                                terminal_id: id.clone(),
                                cols,
                                rows,
                            })?;
                            sizes.insert(id.clone(), (cols, rows));
                        }
                        let v = screen_client.call(OfficeRequestKind::TerminalViewport {
                            terminal_id: id.clone(),
                            scrollback: offset,
                        })?;
                        serde_json::from_value(v).map_err(Into::into)
                    })()
                    .map_err(|e: crate::foundation::OfficeError| e.to_string());
                    let _ = screen_tx.try_send((id, result));
                }
            }
        });
        let halt = stop.clone();
        let bytes = queued.clone();
        std::thread::spawn(move || {
            while !halt.load(Ordering::Relaxed) {
                let Ok((id, data)) = input_rx.recv_timeout(std::time::Duration::from_millis(100))
                else {
                    continue;
                };
                let result = input_client.call(OfficeRequestKind::TerminalInput {
                    terminal_id: id,
                    bytes_hex: crate::office::hex_encode(&data),
                });
                bytes.fetch_sub(data.len(), Ordering::Relaxed);
                if let Err(e) = result {
                    let _ = error_tx.try_send(format!("input failed: {e}"));
                }
            }
        });
        Ok(Self {
            models,
            screens,
            errors,
            viewports,
            input,
            queued,
            stop,
        })
    }
    pub fn request(&self, views: Vec<Viewport>) {
        let _ = self.viewports.try_send(views);
    }
    pub fn send(&self, id: String, data: Vec<u8>) -> Result<(), String> {
        let len = data.len();
        // CAS loop instead of `fetch_update`: the std method was renamed
        // to `try_update` in newer Rust, so `fetch_update` is deprecated
        // (clippy -D warnings) on CI's toolchain while older stables lack
        // `try_update`. A plain compare_exchange compiles everywhere.
        let mut observed = self.queued.load(Ordering::Relaxed);
        loop {
            let next = observed + len;
            if next > 64 * 1024 {
                return Err("input queue full (64 KiB); input was not sent".into());
            }
            match self
                .queued
                .compare_exchange(observed, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(now) => observed = now,
            }
        }
        if self.input.try_send((id, data)).is_err() {
            self.queued.fetch_sub(len, Ordering::Relaxed);
            return Err("input queue full (32 events); input was not sent".into());
        }
        Ok(())
    }
}
impl Drop for Polling {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
