//! Running port forwards. A forward runs until it is stopped, its connection
//! fails or the app quits, independent of any terminal tab. Every change of
//! state is emitted as a `port-forward:status` event.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tauri::{AppHandle, Emitter};

use crate::models::{PortForward, PortForwardStatus};

pub const STATUS_EVENT: &str = "port-forward:status";

struct Tunnel {
    /// Tells this run apart from a later run of the same forward.
    generation: u64,
    stop: Box<dyn Fn() + Send + Sync>,
}

#[derive(Default)]
pub struct TunnelRegistry {
    tunnels: Mutex<HashMap<String, Tunnel>>,
    statuses: Mutex<HashMap<String, PortForwardStatus>>,
    generation: Mutex<u64>,
}

impl TunnelRegistry {
    pub fn statuses(&self) -> Vec<PortForwardStatus> {
        self.statuses
            .lock()
            .map(|statuses| statuses.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn start(self: &Arc<Self>, app: &AppHandle, forward: PortForward) -> Result<(), String> {
        // Register the run before launching it, so a second start is a no-op and
        // the run's own reports (which can arrive before `launch` returns) find it.
        let generation = {
            let mut tunnels = self.tunnels.lock().map_err(|_| "端口转发状态不可用")?;
            if tunnels.contains_key(&forward.id) {
                return Ok(());
            }
            let mut generation = self.generation.lock().map_err(|_| "端口转发状态不可用")?;
            *generation += 1;
            tunnels.insert(
                forward.id.clone(),
                Tunnel {
                    generation: *generation,
                    stop: Box::new(|| {}),
                },
            );
            *generation
        };
        self.set_status(app, &forward.id, "starting", None);
        match self.launch(app, &forward, generation) {
            Ok(stop) => {
                let mut tunnels = self.tunnels.lock().map_err(|_| "端口转发状态不可用")?;
                match tunnels.get_mut(&forward.id) {
                    Some(tunnel) if tunnel.generation == generation => tunnel.stop = stop,
                    // Stopped, or already ended, while launching.
                    _ => {
                        drop(tunnels);
                        stop();
                    }
                }
                Ok(())
            }
            Err(message) => {
                self.finish(app, &forward.id, generation, message.clone());
                Err(message)
            }
        }
    }

    pub fn stop(&self, app: &AppHandle, id: &str) {
        let tunnel = self
            .tunnels
            .lock()
            .ok()
            .and_then(|mut tunnels| tunnels.remove(id));
        if let Some(tunnel) = tunnel {
            (tunnel.stop)();
        }
        self.set_status(app, id, "stopped", None);
    }

    /// Stop every forward, e.g. when the app quits.
    pub fn stop_all(&self) {
        let tunnels: Vec<Tunnel> = self
            .tunnels
            .lock()
            .map(|mut tunnels| tunnels.drain().map(|(_, tunnel)| tunnel).collect())
            .unwrap_or_default();
        for tunnel in tunnels {
            (tunnel.stop)();
        }
    }

    fn is_current(&self, id: &str, generation: u64) -> bool {
        self.tunnels
            .lock()
            .map(|tunnels| {
                tunnels
                    .get(id)
                    .is_some_and(|tunnel| tunnel.generation == generation)
            })
            .unwrap_or(false)
    }

    fn mark_running(&self, app: &AppHandle, id: &str, generation: u64) {
        if self.is_current(id, generation) {
            self.set_status(app, id, "running", None);
        }
    }

    /// A run ended on its own (not through `stop`): report why.
    fn finish(&self, app: &AppHandle, id: &str, generation: u64, reason: String) {
        let removed = self
            .tunnels
            .lock()
            .map(|mut tunnels| {
                if tunnels
                    .get(id)
                    .is_some_and(|tunnel| tunnel.generation == generation)
                {
                    tunnels.remove(id);
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        if removed {
            self.set_status(app, id, "error", Some(reason));
        }
    }

    fn set_status(&self, app: &AppHandle, id: &str, state: &str, message: Option<String>) {
        let status = PortForwardStatus {
            id: id.to_string(),
            state: state.to_string(),
            message,
        };
        if let Ok(mut statuses) = self.statuses.lock() {
            statuses.insert(id.to_string(), status.clone());
        }
        let _ = app.emit(STATUS_EVENT, status);
    }

    fn launch(
        self: &Arc<Self>,
        app: &AppHandle,
        forward: &PortForward,
        generation: u64,
    ) -> Result<Box<dyn Fn() + Send + Sync>, String> {
        let id = forward.id.clone();
        let (registry, running_app, running_id) = (self.clone(), app.clone(), id.clone());
        let on_running = Box::new(move || {
            registry.mark_running(&running_app, &running_id, generation);
        });
        let (registry, ended_app) = (self.clone(), app.clone());
        let on_end = Box::new(move |reason: String| {
            registry.finish(&ended_app, &id, generation, reason);
        });
        crate::transport::start_forward(app, forward, on_running, on_end)
    }
}
