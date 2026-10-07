//! Test double for [`RideProvider`]: quotes a fixed fare, counts requests, and reports whatever
//! status a test sets.

use std::sync::Mutex;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{Duration, Utc};

use super::domain::{Driver, FareQuote, Place, Ride, RideStatus, Vehicle};
use super::ports::RideProvider;

pub struct MockRideProvider {
    status: Mutex<RideStatus>,
    quotes: Mutex<usize>,
    requests: Mutex<usize>,
    fail_requests: bool,
}

impl Default for MockRideProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl MockRideProvider {
    pub fn new() -> Self {
        Self {
            status: Mutex::new(RideStatus::Processing),
            quotes: Mutex::new(0),
            requests: Mutex::new(0),
            fail_requests: false,
        }
    }

    /// Every `request` errors, as a provider outage would.
    pub fn failing_requests(mut self) -> Self {
        self.fail_requests = true;
        self
    }

    pub fn set_status(&self, status: RideStatus) {
        *self.status.lock().unwrap() = status;
    }

    /// How many times `quote` was called.
    pub fn quotes(&self) -> usize {
        *self.quotes.lock().unwrap()
    }

    /// How many times `request` was called.
    pub fn requests(&self) -> usize {
        *self.requests.lock().unwrap()
    }

    fn ride(&self) -> Ride {
        Ride {
            request_id: "req-1".to_string(),
            status: self.status.lock().unwrap().clone(),
            driver: Some(Driver {
                name: "Amina".to_string(),
                phone_number: None,
                rating: Some(4.9),
            }),
            vehicle: Some(Vehicle {
                make: "Toyota".to_string(),
                model: "Axio".to_string(),
                license_plate: "KDA 123A".to_string(),
            }),
            pickup_eta_mins: Some(4),
        }
    }
}

#[async_trait]
impl RideProvider for MockRideProvider {
    fn name(&self) -> &'static str {
        "mock"
    }

    async fn quote(
        &self,
        _profile_id: &str,
        _pickup: &Place,
        _dropoff: &Place,
    ) -> Result<FareQuote> {
        *self.quotes.lock().unwrap() += 1;
        Ok(FareQuote {
            fare_id: "fare-1".to_string(),
            display: "KES 1,250".to_string(),
            currency_code: "KES".to_string(),
            expires_at: Utc::now() + Duration::days(3650),
            pickup_eta_mins: Some(4),
            product_id: None,
        })
    }

    async fn request(
        &self,
        _profile_id: &str,
        _pickup: &Place,
        _dropoff: &Place,
        _quote: &FareQuote,
    ) -> Result<Ride> {
        *self.requests.lock().unwrap() += 1;
        if self.fail_requests {
            return Err(anyhow!("provider unavailable"));
        }
        Ok(self.ride())
    }

    async fn ride(&self, _profile_id: &str, _request_id: &str) -> Result<Ride> {
        Ok(self.ride())
    }

    async fn cancel(&self, _profile_id: &str, _request_id: &str) -> Result<()> {
        self.set_status(RideStatus::RiderCanceled);
        Ok(())
    }
}
