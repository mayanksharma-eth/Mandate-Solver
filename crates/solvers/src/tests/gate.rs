//! A test-only rendezvous point inside the Mandate handlers.
//!
//! A gated request parks in the handler — holding whatever middleware resources
//! it acquired on the way in, such as a concurrency permit — until the test
//! releases it. That makes overload and timeout behaviour observable without
//! sleeping and hoping.
//!
//! Gates are keyed by the request's `liquiditySource.name`, so a test gates its
//! own requests by naming a source only it uses.

use {
    std::{
        collections::HashMap,
        sync::{Arc, Mutex, OnceLock},
    },
    tokio::sync::Semaphore,
};

pub struct Gate {
    arrived: Semaphore,
    release: Semaphore,
}

impl Gate {
    /// Waits until one more request has parked in the handler.
    pub async fn arrived(&self) {
        self.arrived.acquire().await.unwrap().forget();
    }

    /// Lets one parked request continue.
    pub fn release(&self) {
        self.release.add_permits(1);
    }
}

fn gates() -> &'static Mutex<HashMap<String, Arc<Gate>>> {
    static GATES: OnceLock<Mutex<HashMap<String, Arc<Gate>>>> = OnceLock::new();
    GATES.get_or_init(Default::default)
}

/// Gates requests naming `source` as their liquidity source.
pub fn install(source: &str) -> Arc<Gate> {
    let gate = Arc::new(Gate {
        arrived: Semaphore::new(0),
        release: Semaphore::new(0),
    });
    gates()
        .lock()
        .unwrap()
        .insert(source.to_owned(), gate.clone());
    gate
}

/// Parks the request if its liquidity source is gated. Returns immediately if a
/// gated request is dropped, e.g. because it timed out.
pub async fn wait(source: &str) {
    let gate = gates().lock().unwrap().get(source).cloned();
    let Some(gate) = gate else {
        return;
    };
    gate.arrived.add_permits(1);
    gate.release.acquire().await.unwrap().forget();
}
