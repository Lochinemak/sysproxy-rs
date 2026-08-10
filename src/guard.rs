use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use log::{debug, error};
use tokio::sync::{Notify, mpsc, watch};

use crate::{Autoproxy, Sysproxy};

#[derive(Debug, PartialEq, Clone)]
pub enum GuardType {
    None,
    Sysproxy(Sysproxy),
    Autoproxy(Autoproxy),
}

#[repr(u8)]
#[derive(Clone, Copy, Debug)]
pub enum GuardState {
    Running = 0,
    Stopped,
    NeedRestart,
    Pending,
}

impl GuardState {
    #[inline]
    pub const fn from_u8(value: u8) -> Self {
        match value {
            0 => GuardState::Running,
            1 => GuardState::Stopped,
            2 => GuardState::NeedRestart,
            3 => GuardState::Pending,
            _ => GuardState::Stopped,
        }
    }

    #[inline]
    pub const fn to_u8(&self) -> u8 {
        *self as u8
    }

    #[inline]
    pub const fn is_running(&self) -> bool {
        matches!(self, GuardState::Running)
    }

    #[inline]
    pub const fn is_stopped(&self) -> bool {
        matches!(self, GuardState::Stopped)
    }

    #[inline]
    pub const fn is_need_restart(&self) -> bool {
        matches!(self, GuardState::NeedRestart)
    }

    #[inline]
    pub const fn is_pendding(&self) -> bool {
        matches!(self, GuardState::Pending)
    }
}

impl fmt::Display for GuardState {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GuardState::Running => write!(f, "Running"),
            GuardState::Stopped => write!(f, "Stopped"),
            GuardState::NeedRestart => write!(f, "NeedRestart"),
            GuardState::Pending => write!(f, "Pendding"),
        }
    }
}

/// What one guard check did.
///
/// The monitor reports every check; callers decide failure policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardOutcome {
    /// The OS already matched the target; nothing was written.
    Matched,
    /// The OS had drifted and was rewritten successfully.
    Rewritten,
    /// The current OS state could not be read, so drift could not be judged.
    ReadFailed(String),
    /// The OS had drifted, and rewriting it failed.
    WriteFailed(String),
}

/// Observes when no monitor loop is running.
///
/// The shared signal supports multiple waiters and survives timeouts.
#[must_use = "a shutdown is only requested until it is awaited"]
pub struct GuardIdle {
    idle: watch::Receiver<bool>,
}

impl GuardIdle {
    /// Wait until no loop is running; return `false` on timeout.
    #[inline]
    pub async fn wait_timeout(mut self, timeout: Duration) -> bool {
        matches!(
            tokio::time::timeout(timeout, self.idle.wait_for(|idle| *idle)).await,
            Ok(Ok(_))
        )
    }
}

/// Marks a run stopped and idle on every task exit path, including pre-poll drops.
struct RunGuard {
    guard_stat: Arc<AtomicU8>,
    idle: Arc<watch::Sender<bool>>,
}

impl Drop for RunGuard {
    #[inline]
    fn drop(&mut self) {
        // Publish Stopped before waking idle waiters.
        self.guard_stat
            .store(GuardState::Stopped as u8, Ordering::Release);
        self.idle.send_replace(true);
    }
}

#[derive(Clone)]
struct TaskConfig {
    guard_type: GuardType,
    interval: Duration,
    outcomes: Option<mpsc::UnboundedSender<GuardOutcome>>,
}

pub struct GuardMonitor {
    guard_type: GuardType,
    interval: Duration,
    notify: Arc<Notify>,
    guard_stat: Arc<AtomicU8>,
    outcomes: Option<mpsc::UnboundedSender<GuardOutcome>>,
    /// `true` while no loop is running.
    idle: Arc<watch::Sender<bool>>,
}

impl Drop for GuardMonitor {
    #[inline]
    fn drop(&mut self) {
        self.stop();
    }
}

impl GuardMonitor {
    #[inline]
    pub fn new(guard_type: GuardType, interval: Duration) -> Self {
        debug!("Create GuardMonitor with interval: {:?}", interval);
        debug!(
            "GuardType: {:?}",
            match &guard_type {
                GuardType::Sysproxy(sysproxy) => format!("Sysproxy: {:?}", sysproxy),
                GuardType::Autoproxy(autoproxy) => format!("Autoproxy: {:?}", autoproxy),
                GuardType::None => "None".to_string(),
            }
        );
        Self {
            guard_type,
            interval,
            notify: Arc::new(Notify::new()),
            guard_stat: Arc::new(AtomicU8::new(GuardState::Stopped as u8)),
            outcomes: None,
            idle: Arc::new(watch::Sender::new(true)),
        }
    }

    /// Receive the outcome of every check the monitor performs.
    ///
    /// Set before [`start`](Self::start); dropping the receiver only stops reporting.
    #[inline]
    pub fn set_outcome_sender(&mut self, sender: Option<mpsc::UnboundedSender<GuardOutcome>>) {
        self.outcomes = sender;
    }

    /// Observe when no loop is running, without asking one to stop.
    #[inline]
    pub fn idle(&self) -> GuardIdle {
        GuardIdle {
            idle: self.idle.subscribe(),
        }
    }

    /// Ask the monitor to stop and hand back an observer for when it has actually finished.
    ///
    /// Call synchronously, release any monitor lock, then await the observer.
    #[inline]
    #[must_use = "shutdown only requests a stop; await the returned observer to know it finished"]
    pub fn shutdown(&self) -> GuardIdle {
        self.stop();
        self.idle()
    }

    #[inline]
    pub fn get_state(&self) -> GuardState {
        GuardState::from_u8(self.guard_stat.load(Ordering::Acquire))
    }

    #[inline]
    fn set_state(&self, new: GuardState) {
        debug!("GuardMonitor setting state to: {:?}", new);
        self.guard_stat.store(new as u8, Ordering::Release);
    }

    #[inline]
    pub fn set_interval(&mut self, interval: Duration) {
        debug!("Setting interval: {:?}", interval);
        let should_restart = !self.get_state().is_stopped() && self.interval != interval;
        if should_restart {
            debug!("Interval changed while running, monitor should be restarted.");
            self.set_state(GuardState::NeedRestart);
        }
        self.interval = interval;
    }

    #[inline]
    pub fn set_guard_type(&mut self, guard_type: GuardType) {
        debug!("Setting guard_type: {:?}", guard_type);
        let should_restart = !self.get_state().is_stopped() && self.guard_type != guard_type;
        if should_restart {
            debug!("GuardType changed while running, monitor should be restarted.");
            self.set_state(GuardState::NeedRestart);
        }
        self.guard_type = guard_type;
    }

    #[inline]
    fn guard_sysproxy_static(sysproxy: &Sysproxy) -> GuardOutcome {
        // Report read failures instead of silently skipping checks.
        let actually_sysproxy = match Sysproxy::get_system_proxy() {
            Ok(actual) => actual,
            Err(e) => {
                error!("Failed to read system proxy: {:?}", e);
                return GuardOutcome::ReadFailed(e.to_string());
            }
        };

        if &actually_sysproxy == sysproxy {
            return GuardOutcome::Matched;
        }

        debug!(
            "Sysproxy settings do not match! Expected: {:?}, Actual: {:?}",
            sysproxy, actually_sysproxy
        );
        debug!("Resetting Sysproxy to: {:?}", sysproxy);
        match sysproxy.set_system_proxy() {
            Ok(()) => GuardOutcome::Rewritten,
            Err(e) => {
                error!("Failed to set system proxy: {:?}", e);
                GuardOutcome::WriteFailed(e.to_string())
            }
        }
    }

    #[inline]
    fn guard_autoproxy_static(autoproxy: &Autoproxy) -> GuardOutcome {
        let actually_autoproxy = match Autoproxy::get_auto_proxy() {
            Ok(actual) => actual,
            Err(e) => {
                error!("Failed to read auto proxy: {:?}", e);
                return GuardOutcome::ReadFailed(e.to_string());
            }
        };

        if &actually_autoproxy == autoproxy {
            return GuardOutcome::Matched;
        }

        debug!(
            "Autoproxy settings do not match! Expected: {:?}, Actual: {:?}",
            autoproxy, actually_autoproxy
        );
        debug!("Resetting Autoproxy to: {:?}", autoproxy);
        match autoproxy.set_auto_proxy() {
            Ok(()) => GuardOutcome::Rewritten,
            Err(e) => {
                error!("Failed to set auto proxy: {:?}", e);
                GuardOutcome::WriteFailed(e.to_string())
            }
        }
    }

    #[inline]
    /// Start the monitor loop. Returns whether a loop was actually started.
    ///
    /// Returns `false` if a loop is active or the previous run is still draining.
    pub fn start(&self) -> bool {
        debug!("Starting GuardMonitor...");

        let state = self.get_state();
        if state.is_running() || state.is_pendding() {
            debug!("GuardMonitor is already running or pending, skipping start.");
            return false;
        }

        if self.get_state().is_need_restart() {
            debug!("GuardMonitor is in NeedRestart state, stopping and restarting...");
            self.stop();
            std::thread::sleep(Duration::from_millis(50));
        }

        // Atomically reserve the idle slot; Stopped runs may still be draining.
        let claimed = self.idle.send_if_modified(|idle| {
            if *idle {
                *idle = false;
                true
            } else {
                false
            }
        });
        if !claimed {
            log::warn!("GuardMonitor's previous run has not finished yet; not starting.");
            return false;
        }

        if self
            .guard_stat
            .compare_exchange(
                GuardState::Stopped as u8,
                GuardState::Pending as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            debug!("GuardMonitor is not in Stopped state, skipping start.");
            self.idle.send_replace(true);
            return false;
        }

        let config = TaskConfig {
            guard_type: self.guard_type.clone(),
            interval: self.interval,
            outcomes: self.outcomes.clone(),
        };
        let guard_stat = Arc::clone(&self.guard_stat);
        let notify = Arc::clone(&self.notify);
        let run_guard = RunGuard {
            guard_stat: Arc::clone(&self.guard_stat),
            idle: Arc::clone(&self.idle),
        };
        tokio::spawn(async move {
            // Captured so pre-poll task drops still release the run.
            let _run_guard = run_guard;
            Self::run_monitor_loop(guard_stat, notify, config).await;
        });

        debug!("GuardMonitor spawned successfully.");
        true
    }

    #[inline]
    async fn run_monitor_loop(guard_stat: Arc<AtomicU8>, notify: Arc<Notify>, config: TaskConfig) {
        let mut interval = tokio::time::interval(config.interval);
        debug!("GuardMonitor started with interval: {:?}", config.interval);

        // Claim Pending only if no stop arrived after spawning.
        if guard_stat
            .compare_exchange(
                GuardState::Pending as u8,
                GuardState::Running as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            debug!("GuardMonitor was stopped before it started; exiting without checking.");
            return;
        }

        loop {
            let state = GuardState::from_u8(guard_stat.load(Ordering::Acquire));
            if state.is_stopped() {
                break;
            }
            if state.is_need_restart() {
                debug!("GuardMonitor detected NeedRestart state, stopping...");
                break;
            }

            tokio::select! {
                _ = interval.tick() => {
                    let outcome = match &config.guard_type {
                        GuardType::Sysproxy(sysproxy) => {
                            debug!("GuardMonitor checking Sysproxy: {:?}", sysproxy);
                            Some(Self::guard_sysproxy_static(sysproxy))
                        }
                        GuardType::Autoproxy(autoproxy) => {
                            debug!("GuardMonitor checking Autoproxy: {:?}", autoproxy);
                            Some(Self::guard_autoproxy_static(autoproxy))
                        }
                        GuardType::None => {
                            debug!("GuardMonitor has no GuardType set, skipping check.");
                            None
                        }
                    };

                    // A dropped receiver just ends the reporting; it must not end the guard.
                    if let (Some(outcome), Some(sender)) = (outcome, config.outcomes.as_ref()) {
                        let _ = sender.send(outcome);
                    }
                }
                _ = notify.notified() => {
                    debug!("GuardMonitor received stop notification.");
                    break;
                }
            }
        }
    }

    #[inline]
    pub fn stop(&self) {
        debug!("Stopping GuardMonitor...");

        // Pending tasks must observe Stopped before claiming the run.
        if self.get_state().is_stopped() {
            debug!("GuardMonitor is already stopped, skipping stop.");
            return;
        }

        self.set_state(GuardState::Stopped);
        self.notify.notify_waiters();

        debug!("GuardMonitor has been stopped.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_guard_monitor_creation() {
        let target_auto_proxy = Autoproxy {
            url: "http://example.com/proxy.pac".to_string(),
            enable: true,
        };

        let guard_monitor = GuardMonitor::new(
            GuardType::Autoproxy(target_auto_proxy),
            Duration::from_secs(3),
        );

        assert!(!guard_monitor.get_state().is_running());
    }

    #[test]
    fn test_guard_monitor_sysproxy_creation() {
        let sysproxy = Sysproxy {
            enable: true,
            host: "127.0.0.1".to_string(),
            port: 8080,
            bypass: "localhost".to_string(),
        };

        let guard_monitor =
            GuardMonitor::new(GuardType::Sysproxy(sysproxy), Duration::from_secs(5));

        assert!(!guard_monitor.get_state().is_running());
    }

    #[tokio::test]
    async fn test_guard_monitor_start_stop() {
        // None avoids touching the machine on the immediate first tick.
        let guard_monitor = GuardMonitor::new(GuardType::None, Duration::from_millis(100));

        let monitor = Arc::new(guard_monitor);
        let monitor_clone = Arc::clone(&monitor);

        tokio::spawn(async move {
            let _ = &monitor_clone;
            tokio::time::sleep(Duration::from_secs(10)).await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        monitor.start();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(monitor.get_state().is_running());

        monitor.stop();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!monitor.get_state().is_running());
    }

    #[tokio::test]
    async fn test_set_interval() {
        let target_auto_proxy = Autoproxy {
            url: "http://example.com/proxy.pac".to_string(),
            enable: true,
        };

        let mut guard_monitor = GuardMonitor::new(
            GuardType::Autoproxy(target_auto_proxy),
            Duration::from_secs(3),
        );

        guard_monitor.set_interval(Duration::from_secs(5));
    }

    #[tokio::test]
    async fn test_set_guard_type() {
        let initial_auto_proxy = Autoproxy {
            url: "http://example.com/proxy.pac".to_string(),
            enable: true,
        };

        let mut guard_monitor = GuardMonitor::new(
            GuardType::Autoproxy(initial_auto_proxy),
            Duration::from_secs(3),
        );

        let new_sysproxy = Sysproxy {
            enable: true,
            host: "192.168.1.1".to_string(),
            port: 3128,
            bypass: "*.local".to_string(),
        };

        guard_monitor.set_guard_type(GuardType::Sysproxy(new_sysproxy));
    }

    #[test]
    fn test_guard_autoproxy_matching() {
        let target_auto_proxy = Autoproxy {
            url: "http://example.com/proxy.pac".to_string(),
            enable: true,
        };

        let guard_monitor = GuardMonitor::new(
            GuardType::Autoproxy(target_auto_proxy),
            Duration::from_secs(1),
        );

        // This will call guard_autoproxy internally
        if let GuardType::Autoproxy(ref autoproxy) = guard_monitor.guard_type {
            assert_eq!(autoproxy.url, "http://example.com/proxy.pac");
            assert!(autoproxy.enable);
        }
    }

    #[test]
    fn test_guard_sysproxy_matching() {
        let sysproxy = Sysproxy {
            enable: true,
            host: "proxy.example.com".to_string(),
            port: 8888,
            bypass: "localhost,127.0.0.1".to_string(),
        };

        let guard_monitor =
            GuardMonitor::new(GuardType::Sysproxy(sysproxy), Duration::from_secs(2));

        if let GuardType::Sysproxy(ref proxy) = guard_monitor.guard_type {
            assert_eq!(proxy.host, "proxy.example.com");
            assert_eq!(proxy.port, 8888);
            assert!(proxy.enable);
        }
    }

    #[test]
    fn test_guard_monitor_disabled_autoproxy() {
        let disabled_autoproxy = Autoproxy {
            url: "http://example.com/proxy.pac".to_string(),
            enable: false,
        };

        let guard_monitor = GuardMonitor::new(
            GuardType::Autoproxy(disabled_autoproxy),
            Duration::from_secs(1),
        );

        if let GuardType::Autoproxy(ref autoproxy) = guard_monitor.guard_type {
            assert!(!autoproxy.enable);
        }
    }

    #[test]
    fn test_guard_monitor_disabled_sysproxy() {
        let disabled_sysproxy = Sysproxy {
            enable: false,
            host: "127.0.0.1".to_string(),
            port: 8080,
            bypass: "localhost".to_string(),
        };

        let guard_monitor = GuardMonitor::new(
            GuardType::Sysproxy(disabled_sysproxy),
            Duration::from_secs(1),
        );

        if let GuardType::Sysproxy(ref proxy) = guard_monitor.guard_type {
            assert!(!proxy.enable);
        }
    }

    // State Machine Tests
    #[test]
    fn test_initial_state() {
        let monitor = GuardMonitor::new(GuardType::None, Duration::from_secs(1));
        assert!(monitor.get_state().is_stopped());
        assert!(!monitor.get_state().is_running());
        assert!(!monitor.get_state().is_pendding());
        assert!(!monitor.get_state().is_need_restart());
    }

    #[tokio::test]
    async fn test_state_transition_stopped_to_running() {
        let monitor = Arc::new(GuardMonitor::new(
            GuardType::None,
            Duration::from_millis(50),
        ));

        assert!(monitor.get_state().is_stopped());

        monitor.start();

        // Wait for state to become Running
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(monitor.get_state().is_running());

        monitor.stop();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(monitor.get_state().is_stopped());
    }

    #[tokio::test]
    async fn test_cannot_start_while_running() {
        let monitor = Arc::new(GuardMonitor::new(
            GuardType::None,
            Duration::from_millis(50),
        ));

        monitor.start();

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(monitor.get_state().is_running());

        // Try to start again - should be rejected
        monitor.start(); // This should return immediately

        // Should still be running from the first start
        assert!(monitor.get_state().is_running());

        monitor.stop();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn test_cannot_start_while_pending() {
        let monitor = Arc::new(GuardMonitor::new(
            GuardType::None,
            Duration::from_millis(100),
        ));

        monitor.start();

        // Immediately try to start again (during Pendding state)
        tokio::time::sleep(Duration::from_millis(5)).await;
        let state = monitor.get_state();
        assert!(state.is_pendding() || state.is_running());

        monitor.start(); // Should be rejected

        monitor.stop();
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    #[tokio::test]
    async fn test_stop_while_pending() {
        let monitor = Arc::new(GuardMonitor::new(
            GuardType::None,
            Duration::from_millis(200),
        ));

        monitor.start();

        // Stop immediately while in Pendding state
        tokio::time::sleep(Duration::from_millis(5)).await;
        monitor.stop();

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(monitor.get_state().is_stopped());
    }

    #[tokio::test]
    async fn test_multiple_stop_calls() {
        let monitor = Arc::new(GuardMonitor::new(
            GuardType::None,
            Duration::from_millis(50),
        ));

        monitor.start();

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(monitor.get_state().is_running());

        // First stop
        monitor.stop();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(monitor.get_state().is_stopped());

        // Second stop - should be safe and do nothing
        monitor.stop();
        assert!(monitor.get_state().is_stopped());
    }

    #[tokio::test]
    async fn test_need_restart_state_stops_monitor() {
        // None keeps this state-machine test off the real proxy settings.
        let monitor = GuardMonitor::new(GuardType::None, Duration::from_millis(50));

        let monitor_arc = Arc::new(monitor);

        monitor_arc.start();

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(monitor_arc.get_state().is_running());

        // Simulate setting NeedRestart state
        monitor_arc.set_state(GuardState::NeedRestart);

        // Monitor should stop itself when it detects NeedRestart
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(monitor_arc.get_state().is_stopped());
    }

    #[tokio::test]
    async fn test_set_interval_while_running() {
        let monitor = GuardMonitor::new(GuardType::None, Duration::from_millis(50));

        let monitor_arc = Arc::new(monitor);

        monitor_arc.start();

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(monitor_arc.get_state().is_running());

        // This should set NeedRestart state (requires &mut self, so we can't test directly on Arc)
        // But we can verify the state machine behavior

        monitor_arc.stop();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn test_concurrent_start_attempts() {
        let monitor = Arc::new(GuardMonitor::new(
            GuardType::None,
            Duration::from_millis(100),
        ));

        let mut handles = vec![];

        // Try to start 5 times concurrently
        for _ in 0..5 {
            let monitor_clone = Arc::clone(&monitor);
            let handle = tokio::spawn(async move {
                monitor_clone.start();
            });
            handles.push(handle);
        }

        // Wait a bit for all attempts to register
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Only one should succeed - monitor should be in Running state
        assert!(monitor.get_state().is_running());

        monitor.stop();

        // Wait for all tasks to complete
        for handle in handles {
            let _ = handle.await;
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(monitor.get_state().is_stopped());
    }

    #[tokio::test]
    async fn test_restart_after_stop() {
        let monitor = Arc::new(GuardMonitor::new(
            GuardType::None,
            Duration::from_millis(50),
        ));

        // First start
        monitor.start();

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(monitor.get_state().is_running());

        monitor.stop();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(monitor.get_state().is_stopped());

        // Second start - should work after proper stop
        monitor.start();

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(monitor.get_state().is_running());

        monitor.stop();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(monitor.get_state().is_stopped());
    }

    #[tokio::test]
    async fn test_drop_stops_monitor() {
        let monitor = Arc::new(GuardMonitor::new(
            GuardType::None,
            Duration::from_millis(50),
        ));

        monitor.start();

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(monitor.get_state().is_running());

        // Drop will call stop() implicitly
        drop(monitor);

        tokio::time::sleep(Duration::from_millis(100)).await;
        // Can't check state after drop, but monitor should have stopped gracefully
    }

    #[test]
    fn test_guard_state_conversions() {
        assert_eq!(GuardState::from_u8(0).to_u8(), 0);
        assert_eq!(GuardState::from_u8(1).to_u8(), 1);
        assert_eq!(GuardState::from_u8(2).to_u8(), 2);
        assert_eq!(GuardState::from_u8(3).to_u8(), 3);

        // Invalid value should default to Stopped
        assert!(GuardState::from_u8(99).is_stopped());
    }

    #[test]
    fn test_guard_state_display() {
        assert_eq!(format!("{}", GuardState::Running), "Running");
        assert_eq!(format!("{}", GuardState::Stopped), "Stopped");
        assert_eq!(format!("{}", GuardState::NeedRestart), "NeedRestart");
        assert_eq!(format!("{}", GuardState::Pending), "Pendding");
    }

    #[test]
    fn test_guard_monitor_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<GuardMonitor>();
    }

    #[test]
    fn test_guard_monitor_is_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<GuardMonitor>();
    }

    #[tokio::test]
    async fn test_send_across_threads() {
        let monitor = Arc::new(GuardMonitor::new(
            GuardType::None,
            Duration::from_millis(50),
        ));

        // Spawn multiple tasks across different threads
        let mut handles = vec![];
        for i in 0..3 {
            let monitor_clone = Arc::clone(&monitor);
            let handle = tokio::spawn(async move {
                if i == 0 {
                    // First task starts the monitor
                    monitor_clone.start();
                } else {
                    // Other tasks try to read state
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    let _state = monitor_clone.get_state();
                }
            });
            handles.push(handle);
        }

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(monitor.get_state().is_running());

        monitor.stop();

        for handle in handles {
            let _ = handle.await;
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(monitor.get_state().is_stopped());
    }

    #[tokio::test]
    async fn shutdown_waits_for_the_loop_to_actually_exit() {
        let monitor = GuardMonitor::new(GuardType::None, Duration::from_millis(20));
        monitor.start();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(monitor.get_state().is_running());

        assert!(
            monitor
                .shutdown()
                .wait_timeout(Duration::from_secs(2))
                .await,
            "the loop should have exited well within the timeout"
        );
        assert!(monitor.get_state().is_stopped());
    }

    #[tokio::test]
    async fn a_shutdown_during_the_pending_window_is_not_lost() {
        let monitor = GuardMonitor::new(GuardType::None, Duration::from_millis(20));
        monitor.start();

        // The spawned task has not claimed its slot yet.
        assert!(monitor.get_state().is_pendding());

        assert!(
            monitor
                .shutdown()
                .wait_timeout(Duration::from_secs(2))
                .await,
            "a shutdown requested before the loop started must still resolve"
        );
        assert!(monitor.get_state().is_stopped());
    }

    #[tokio::test]
    async fn shutdown_is_idempotent() {
        let monitor = GuardMonitor::new(GuardType::None, Duration::from_millis(20));
        monitor.start();
        tokio::time::sleep(Duration::from_millis(40)).await;

        assert!(
            monitor
                .shutdown()
                .wait_timeout(Duration::from_secs(2))
                .await
        );
        assert!(
            monitor
                .shutdown()
                .wait_timeout(Duration::from_secs(2))
                .await
        );
    }

    #[tokio::test]
    async fn a_guard_with_nothing_to_watch_reports_nothing() {
        // None tests reporting without touching the machine.
        let (sender, mut outcomes) = mpsc::unbounded_channel();
        let mut monitor = GuardMonitor::new(GuardType::None, Duration::from_millis(10));
        monitor.set_outcome_sender(Some(sender));

        monitor.start();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            monitor
                .shutdown()
                .wait_timeout(Duration::from_secs(2))
                .await
        );

        assert!(
            outcomes.try_recv().is_err(),
            "a guard with no target has nothing to report"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn waiting_on_a_loop_nobody_asked_to_stop_times_out() {
        // Multi-threaded so the loop is running independently.
        let monitor = GuardMonitor::new(GuardType::None, Duration::from_secs(3600));
        monitor.start();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(monitor.get_state().is_running());

        assert!(
            !monitor
                .idle()
                .wait_timeout(Duration::from_millis(100))
                .await,
            "a running loop must not be reported as finished"
        );

        assert!(
            monitor
                .shutdown()
                .wait_timeout(Duration::from_secs(2))
                .await
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_wait_that_timed_out_does_not_poison_later_waits() {
        let monitor = GuardMonitor::new(GuardType::None, Duration::from_secs(3600));
        monitor.start();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(!monitor.idle().wait_timeout(Duration::from_millis(50)).await);

        // A timeout must not consume the shared idle signal.
        assert!(
            monitor
                .shutdown()
                .wait_timeout(Duration::from_secs(2))
                .await
        );
    }

    #[tokio::test]
    async fn start_is_refused_while_the_previous_run_is_still_draining() {
        let monitor = GuardMonitor::new(GuardType::None, Duration::from_millis(20));
        monitor.start();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(monitor.get_state().is_running());

        // Stopped can precede the loop's actual exit.
        monitor.stop();
        assert!(monitor.get_state().is_stopped());

        // Refuse overlapping loops and report the refusal.
        assert!(
            !monitor.start(),
            "a start refused because the previous run is draining must say so"
        );
        assert!(monitor.get_state().is_stopped());

        assert!(
            monitor
                .shutdown()
                .wait_timeout(Duration::from_secs(2))
                .await
        );
    }

    #[tokio::test]
    async fn a_start_that_actually_starts_says_so() {
        let monitor = GuardMonitor::new(GuardType::None, Duration::from_millis(20));

        assert!(monitor.start(), "the first start should succeed");
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(monitor.get_state().is_running());

        assert!(!monitor.start(), "starting a second loop must be refused");

        assert!(
            monitor
                .shutdown()
                .wait_timeout(Duration::from_secs(2))
                .await
        );

        assert!(monitor.start(), "a drained monitor should start again");
        assert!(
            monitor
                .shutdown()
                .wait_timeout(Duration::from_secs(2))
                .await
        );
    }
}
