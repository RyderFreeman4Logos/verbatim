//! Observable bounded resource queues shared by daemon and provider code.
//!
//! A resource queue is intentionally small: it bounds admitted waiters, records
//! queue wait and service timing, and releases capacity through RAII. Callers
//! keep domain work outside this module so resource boundaries stay explicit.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Runtime limits for one bounded resource queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLimitConfig {
    pub capacity: usize,
    pub queue_capacity: usize,
    pub queue_timeout: Duration,
}

impl ResourceLimitConfig {
    pub fn bounded(self) -> Self {
        Self {
            capacity: self.capacity.max(1),
            queue_capacity: self.queue_capacity.max(1),
            queue_timeout: self.queue_timeout.max(Duration::from_millis(1)),
        }
    }
}

/// Low-cardinality resource queue state exposed through daemon status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceQueueSnapshot {
    pub name: String,
    pub kind: String,
    pub capacity: usize,
    pub queue_capacity: usize,
    pub queued: usize,
    pub active: usize,
    pub completed: u64,
    pub errors: u64,
    pub queue_wait_ms_total: u64,
    pub service_ms_total: u64,
    pub last_queue_wait_ms: Option<u64>,
    pub last_service_ms: Option<u64>,
    pub throughput_per_minute: f64,
}

/// Per-task resource timing that can be attached to progress snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResourceProgress {
    pub name: String,
    pub kind: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_wait_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_ms: Option<u64>,
    pub queued: usize,
    pub active: usize,
    pub capacity: usize,
}

impl TaskResourceProgress {
    pub fn from_snapshot(
        snapshot: &ResourceQueueSnapshot,
        state: impl Into<String>,
        queue_wait_ms: Option<u64>,
        service_ms: Option<u64>,
    ) -> Self {
        Self {
            name: snapshot.name.clone(),
            kind: snapshot.kind.clone(),
            state: state.into(),
            queue_wait_ms,
            service_ms,
            queued: snapshot.queued,
            active: snapshot.active,
            capacity: snapshot.capacity,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceQueueError {
    Full {
        name: String,
        kind: String,
        queue_capacity: usize,
    },
    Timeout {
        name: String,
        kind: String,
        timeout: Duration,
    },
}

impl fmt::Display for ResourceQueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full {
                name,
                kind,
                queue_capacity,
            } => write!(
                f,
                "{kind} resource queue '{name}' is full (queue_capacity={queue_capacity})"
            ),
            Self::Timeout {
                name,
                kind,
                timeout,
            } => write!(
                f,
                "{kind} resource queue '{name}' timed out after waiting {}ms",
                timeout.as_millis()
            ),
        }
    }
}

impl std::error::Error for ResourceQueueError {}

#[derive(Debug)]
pub struct ObservableResource {
    name: String,
    kind: String,
    state: Mutex<ResourceState>,
    condvar: Condvar,
    notify: tokio::sync::Notify,
    metrics: ResourceMetrics,
    created_at: Instant,
}

impl ObservableResource {
    pub fn new(
        name: impl Into<String>,
        kind: impl Into<String>,
        config: ResourceLimitConfig,
    ) -> Self {
        let config = config.bounded();
        Self {
            name: name.into(),
            kind: kind.into(),
            state: Mutex::new(ResourceState {
                capacity: config.capacity,
                queue_capacity: config.queue_capacity,
                queue_timeout: config.queue_timeout,
                active: 0,
                queued: 0,
            }),
            condvar: Condvar::new(),
            notify: tokio::sync::Notify::new(),
            metrics: ResourceMetrics::default(),
            created_at: Instant::now(),
        }
    }

    pub fn configure(&self, config: ResourceLimitConfig) {
        let config = config.bounded();
        {
            let mut state = lock_unpoisoned(&self.state);
            state.capacity = config.capacity;
            state.queue_capacity = config.queue_capacity;
            state.queue_timeout = config.queue_timeout;
        }
        self.condvar.notify_all();
        self.notify.notify_waiters();
    }

    pub async fn acquire(self: &Arc<Self>) -> Result<ResourcePermit, ResourceQueueError> {
        let timeout = {
            let state = lock_unpoisoned(&self.state);
            state.queue_timeout
        };
        let wait_started = Instant::now();
        let mut enqueued = false;
        let admitted = tokio::time::timeout(timeout, async {
            loop {
                let notified = self.notify.notified();
                {
                    let mut state = lock_unpoisoned(&self.state);
                    if state.active < state.capacity {
                        state.active += 1;
                        if enqueued {
                            state.queued = state.queued.saturating_sub(1);
                        }
                        let queue_wait_ms = elapsed_ms(wait_started);
                        self.metrics.record_queue_wait(queue_wait_ms);
                        return Ok(ResourcePermit {
                            resource: Arc::clone(self),
                            service_started: Instant::now(),
                            queue_wait_ms,
                        });
                    }
                    if !enqueued {
                        if state.queued >= state.queue_capacity {
                            self.metrics.record_error();
                            return Err(ResourceQueueError::Full {
                                name: self.name.clone(),
                                kind: self.kind.clone(),
                                queue_capacity: state.queue_capacity,
                            });
                        }
                        state.queued += 1;
                        enqueued = true;
                    }
                }
                notified.await;
            }
        })
        .await;

        match admitted {
            Ok(result) => result,
            Err(_) => {
                if enqueued {
                    let mut state = lock_unpoisoned(&self.state);
                    state.queued = state.queued.saturating_sub(1);
                }
                self.metrics.record_error();
                self.condvar.notify_one();
                self.notify.notify_one();
                Err(ResourceQueueError::Timeout {
                    name: self.name.clone(),
                    kind: self.kind.clone(),
                    timeout,
                })
            }
        }
    }

    pub fn acquire_blocking(self: &Arc<Self>) -> Result<ResourcePermit, ResourceQueueError> {
        let timeout = {
            let state = lock_unpoisoned(&self.state);
            state.queue_timeout
        };
        let wait_started = Instant::now();
        let mut enqueued = false;
        let mut state = lock_unpoisoned(&self.state);
        loop {
            if state.active < state.capacity {
                state.active += 1;
                if enqueued {
                    state.queued = state.queued.saturating_sub(1);
                }
                let queue_wait_ms = elapsed_ms(wait_started);
                self.metrics.record_queue_wait(queue_wait_ms);
                return Ok(ResourcePermit {
                    resource: Arc::clone(self),
                    service_started: Instant::now(),
                    queue_wait_ms,
                });
            }

            if !enqueued {
                if state.queued >= state.queue_capacity {
                    self.metrics.record_error();
                    return Err(ResourceQueueError::Full {
                        name: self.name.clone(),
                        kind: self.kind.clone(),
                        queue_capacity: state.queue_capacity,
                    });
                }
                state.queued += 1;
                enqueued = true;
            }

            let elapsed = wait_started.elapsed();
            if elapsed >= timeout {
                state.queued = state.queued.saturating_sub(1);
                self.metrics.record_error();
                self.condvar.notify_one();
                self.notify.notify_one();
                return Err(ResourceQueueError::Timeout {
                    name: self.name.clone(),
                    kind: self.kind.clone(),
                    timeout,
                });
            }

            let remaining = timeout.saturating_sub(elapsed);
            let (next_state, _) = match self.condvar.wait_timeout(state, remaining) {
                Ok(wait_result) => wait_result,
                Err(poisoned) => poisoned.into_inner(),
            };
            state = next_state;
        }
    }

    pub fn snapshot(&self) -> ResourceQueueSnapshot {
        let state = lock_unpoisoned(&self.state);
        let completed = self.metrics.completed.load(Ordering::Relaxed);
        let elapsed_minutes = self.created_at.elapsed().as_secs_f64() / 60.0;
        ResourceQueueSnapshot {
            name: self.name.clone(),
            kind: self.kind.clone(),
            capacity: state.capacity,
            queue_capacity: state.queue_capacity,
            queued: state.queued,
            active: state.active,
            completed,
            errors: self.metrics.errors.load(Ordering::Relaxed),
            queue_wait_ms_total: self.metrics.queue_wait_ms_total.load(Ordering::Relaxed),
            service_ms_total: self.metrics.service_ms_total.load(Ordering::Relaxed),
            last_queue_wait_ms: optional_metric(
                self.metrics.last_queue_wait_ms.load(Ordering::Relaxed),
            ),
            last_service_ms: optional_metric(self.metrics.last_service_ms.load(Ordering::Relaxed)),
            throughput_per_minute: if elapsed_minutes > 0.0 {
                completed as f64 / elapsed_minutes
            } else {
                0.0
            },
        }
    }

    fn release(&self, service_started: Instant) {
        let service_ms = elapsed_ms(service_started);
        self.metrics.record_service(service_ms);
        {
            let mut state = lock_unpoisoned(&self.state);
            state.active = state.active.saturating_sub(1);
        }
        self.condvar.notify_one();
        self.notify.notify_one();
    }
}

#[derive(Debug)]
pub struct ResourcePermit {
    resource: Arc<ObservableResource>,
    service_started: Instant,
    queue_wait_ms: u64,
}

impl ResourcePermit {
    pub fn queue_wait_ms(&self) -> u64 {
        self.queue_wait_ms
    }

    pub fn service_ms(&self) -> u64 {
        elapsed_ms(self.service_started)
    }

    pub fn snapshot(&self) -> ResourceQueueSnapshot {
        self.resource.snapshot()
    }
}

impl Drop for ResourcePermit {
    fn drop(&mut self) {
        self.resource.release(self.service_started);
    }
}

#[derive(Debug)]
struct ResourceState {
    capacity: usize,
    queue_capacity: usize,
    queue_timeout: Duration,
    active: usize,
    queued: usize,
}

#[derive(Debug, Default)]
struct ResourceMetrics {
    completed: AtomicU64,
    errors: AtomicU64,
    queue_wait_ms_total: AtomicU64,
    service_ms_total: AtomicU64,
    last_queue_wait_ms: AtomicU64,
    last_service_ms: AtomicU64,
}

impl ResourceMetrics {
    fn record_queue_wait(&self, queue_wait_ms: u64) {
        self.queue_wait_ms_total
            .fetch_add(queue_wait_ms, Ordering::Relaxed);
        self.last_queue_wait_ms
            .store(queue_wait_ms.saturating_add(1), Ordering::Relaxed);
    }

    fn record_service(&self, service_ms: u64) {
        self.completed.fetch_add(1, Ordering::Relaxed);
        self.service_ms_total
            .fetch_add(service_ms, Ordering::Relaxed);
        self.last_service_ms
            .store(service_ms.saturating_add(1), Ordering::Relaxed);
    }

    fn record_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Debug, Default)]
pub struct GlobalResourceRegistry {
    resources: Mutex<BTreeMap<String, Arc<ObservableResource>>>,
}

impl GlobalResourceRegistry {
    pub fn resource(
        &self,
        name: impl Into<String>,
        kind: impl Into<String>,
        config: ResourceLimitConfig,
    ) -> Arc<ObservableResource> {
        let name = name.into();
        let kind = kind.into();
        let mut resources = lock_unpoisoned(&self.resources);
        let resource = resources
            .entry(name.clone())
            .or_insert_with(|| {
                Arc::new(ObservableResource::new(
                    name.clone(),
                    kind.clone(),
                    config.bounded(),
                ))
            })
            .clone();
        resource.configure(config);
        resource
    }

    pub fn resource_or_insert(
        &self,
        name: impl Into<String>,
        kind: impl Into<String>,
        config: ResourceLimitConfig,
    ) -> Arc<ObservableResource> {
        let name = name.into();
        let kind = kind.into();
        let mut resources = lock_unpoisoned(&self.resources);
        resources
            .entry(name.clone())
            .or_insert_with(|| {
                Arc::new(ObservableResource::new(
                    name.clone(),
                    kind.clone(),
                    config.bounded(),
                ))
            })
            .clone()
    }

    pub fn snapshots(&self) -> Vec<ResourceQueueSnapshot> {
        let resources = lock_unpoisoned(&self.resources);
        resources
            .values()
            .map(|resource| resource.snapshot())
            .collect()
    }
}

pub fn global_resource_registry() -> &'static GlobalResourceRegistry {
    static REGISTRY: OnceLock<GlobalResourceRegistry> = OnceLock::new();
    REGISTRY.get_or_init(GlobalResourceRegistry::default)
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

fn optional_metric(value: u64) -> Option<u64> {
    value.checked_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_queue_reports_wait_active_throughput_and_errors() {
        let resource = Arc::new(ObservableResource::new(
            "test_resource",
            "test_kind",
            ResourceLimitConfig {
                capacity: 1,
                queue_capacity: 1,
                queue_timeout: Duration::from_secs(5),
            },
        ));

        let first = resource.acquire().await.expect("first permit");
        let waiter_resource = Arc::clone(&resource);
        let waiter = tokio::spawn(async move { waiter_resource.acquire().await });

        tokio::time::sleep(Duration::from_millis(25)).await;
        let queued_snapshot = resource.snapshot();
        assert_eq!(queued_snapshot.active, 1);
        assert_eq!(queued_snapshot.queued, 1);

        let full = resource
            .acquire()
            .await
            .expect_err("bounded queue rejects excess waiter");
        assert!(matches!(full, ResourceQueueError::Full { .. }));
        assert_eq!(resource.snapshot().errors, 1);

        drop(first);
        let second = waiter
            .await
            .expect("waiter task joins")
            .expect("waiter acquires after release");
        assert!(second.queue_wait_ms() > 0);
        drop(second);

        let completed_snapshot = resource.snapshot();
        assert_eq!(completed_snapshot.active, 0);
        assert_eq!(completed_snapshot.queued, 0);
        assert_eq!(completed_snapshot.completed, 2);
        assert_eq!(completed_snapshot.errors, 1);
        assert!(completed_snapshot.queue_wait_ms_total > 0);
        assert!(completed_snapshot.service_ms_total > 0);
        assert!(completed_snapshot.last_queue_wait_ms.is_some());
        assert!(completed_snapshot.last_service_ms.is_some());
        assert!(completed_snapshot.throughput_per_minute > 0.0);
    }

    #[tokio::test]
    async fn timeout_removes_waiter_from_queue() {
        let resource = Arc::new(ObservableResource::new(
            "timeout_resource",
            "test_kind",
            ResourceLimitConfig {
                capacity: 1,
                queue_capacity: 1,
                queue_timeout: Duration::from_millis(10),
            },
        ));

        let _first = resource.acquire().await.expect("first permit");
        let timeout = resource.acquire().await.expect_err("waiter times out");

        assert!(matches!(timeout, ResourceQueueError::Timeout { .. }));
        let snapshot = resource.snapshot();
        assert_eq!(snapshot.active, 1);
        assert_eq!(snapshot.queued, 0);
        assert_eq!(snapshot.errors, 1);
    }

    #[test]
    fn blocking_acquire_waits_and_reports_queue_time() {
        let resource = Arc::new(ObservableResource::new(
            "blocking_resource",
            "test_kind",
            ResourceLimitConfig {
                capacity: 1,
                queue_capacity: 1,
                queue_timeout: Duration::from_secs(5),
            },
        ));

        let first = resource.acquire_blocking().expect("first permit");
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            drop(first);
        });

        let second = resource
            .acquire_blocking()
            .expect("second permit waits for release");
        assert!(second.queue_wait_ms() > 0);
        drop(second);
        release.join().expect("release thread joins");

        let snapshot = resource.snapshot();
        assert_eq!(snapshot.completed, 2);
        assert_eq!(snapshot.active, 0);
        assert!(snapshot.queue_wait_ms_total > 0);
        assert!(snapshot.service_ms_total > 0);
    }
}
