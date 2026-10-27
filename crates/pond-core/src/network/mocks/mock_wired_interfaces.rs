//! In-memory [`WiredInterfaces`] standing in for the host's network interfaces.

use std::sync::Mutex;

use async_trait::async_trait;

use crate::network::domain::wired::WiredInterface;
use crate::network::ports::wired_interfaces::WiredInterfaces;

#[derive(Default)]
pub struct MockWiredInterfaces {
    interfaces: Mutex<Vec<WiredInterface>>,
}

impl MockWiredInterfaces {
    pub fn new(interfaces: Vec<WiredInterface>) -> Self {
        Self {
            interfaces: Mutex::new(interfaces),
        }
    }

    pub fn set(&self, interfaces: Vec<WiredInterface>) {
        *self.interfaces.lock().unwrap() = interfaces;
    }
}

#[async_trait]
impl WiredInterfaces for MockWiredInterfaces {
    async fn wired_interfaces(&self) -> Vec<WiredInterface> {
        self.interfaces.lock().unwrap().clone()
    }
}
