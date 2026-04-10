use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::errors::LauncherError;

/// Represents the current state of the launcher application.
///
/// Each variant carries context relevant to that state.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "state", content = "context")]
pub enum LauncherState {
    /// Initial state before any work has been done.
    Uninitialized,

    /// Pier archive is being extracted.
    Extracting {
        /// Human-readable progress message (e.g., "Verifying checksum...")
        message: String,
    },

    /// Extraction and validation complete; ready to start the runtime.
    Prepared,

    /// The vere runtime is being launched.
    Starting,

    /// The vere runtime is running.
    Running {
        /// OS process ID of the vere child process.
        pid: u32,
        /// ISO-8601 timestamp of when the runtime was started.
        started_at: String,
    },

    /// A graceful shutdown is in progress.
    Stopping,

    /// The runtime has been stopped cleanly.
    Stopped,

    /// The runtime exited unexpectedly.
    Crashed {
        /// Exit code from the vere process, if available.
        exit_code: Option<i32>,
        /// Human-readable description of what happened.
        message: String,
    },

    /// No pier found and no bundled archive — user must import one.
    NeedsPier,

    /// An error occurred during preparation or runtime management.
    Error {
        /// Concise error summary for display.
        message: String,
        /// Extended detail (e.g., full error chain) for diagnostics.
        detail: Option<String>,
    },
}

impl LauncherState {
    /// Returns a short label for the current state, useful for logging and display.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Uninitialized => "Uninitialized",
            Self::Extracting { .. } => "Extracting",
            Self::Prepared => "Prepared",
            Self::Starting => "Starting",
            Self::Running { .. } => "Running",
            Self::Stopping => "Stopping",
            Self::Stopped => "Stopped",
            Self::NeedsPier => "NeedsPier",
            Self::Crashed { .. } => "Crashed",
            Self::Error { .. } => "Error",
        }
    }

    /// Returns the set of states this state is allowed to transition to.
    fn allowed_transitions(&self) -> &'static [&'static str] {
        match self {
            Self::Uninitialized => &["Extracting", "Prepared", "NeedsPier", "Error"],
            Self::Extracting { .. } => &["Extracting", "Prepared", "NeedsPier", "Error"],
            Self::Prepared => &["Starting", "Error", "Uninitialized"],
            Self::Starting => &["Running", "Crashed", "Error"],
            Self::Running { .. } => &["Stopping", "Stopped", "Crashed", "Error"],
            Self::Stopping => &["Stopped", "Crashed", "Error"],
            Self::Stopped => &["Starting", "Uninitialized"],
            Self::NeedsPier => &["Extracting", "Prepared", "Error", "Uninitialized"],
            Self::Crashed { .. } => &["Starting", "Uninitialized"],
            Self::Error { .. } => &["Uninitialized", "Extracting", "Prepared", "Starting"],
        }
    }

    /// Returns true if transitioning from `self` to `target` is valid.
    pub fn can_transition_to(&self, target: &LauncherState) -> bool {
        self.allowed_transitions().contains(&target.label())
    }
}

/// Thread-safe state machine that enforces valid transitions.
#[derive(Debug, Clone)]
pub struct StateMachine {
    inner: Arc<Mutex<LauncherState>>,
}

impl StateMachine {
    /// Creates a new state machine starting in `Uninitialized`.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(LauncherState::Uninitialized)),
        }
    }

    /// Returns a clone of the current state.
    pub fn current(&self) -> LauncherState {
        self.inner.lock().unwrap().clone()
    }

    /// Attempts to transition to `next`. Returns `Ok(())` if the transition is
    /// valid, or `Err(LauncherError::InvalidTransition)` if not.
    pub fn transition(&self, next: LauncherState) -> Result<(), LauncherError> {
        let mut current = self.inner.lock().unwrap();
        if current.can_transition_to(&next) {
            *current = next;
            Ok(())
        } else {
            Err(LauncherError::InvalidTransition {
                from: current.label().to_string(),
                to: next.label().to_string(),
            })
        }
    }

    /// Forces the state to `Error` regardless of current state.
    /// This is an escape hatch for truly unexpected failures.
    pub fn force_error(&self, message: String, detail: Option<String>) {
        let mut current = self.inner.lock().unwrap();
        *current = LauncherState::Error { message, detail };
    }
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- LauncherState label tests --

    #[test]
    fn labels_are_correct() {
        assert_eq!(LauncherState::Uninitialized.label(), "Uninitialized");
        assert_eq!(
            LauncherState::Extracting {
                message: String::new()
            }
            .label(),
            "Extracting"
        );
        assert_eq!(LauncherState::Prepared.label(), "Prepared");
        assert_eq!(LauncherState::Starting.label(), "Starting");
        assert_eq!(
            LauncherState::Running {
                pid: 1,
                started_at: String::new()
            }
            .label(),
            "Running"
        );
        assert_eq!(LauncherState::Stopping.label(), "Stopping");
        assert_eq!(LauncherState::Stopped.label(), "Stopped");
        assert_eq!(
            LauncherState::Crashed {
                exit_code: None,
                message: String::new()
            }
            .label(),
            "Crashed"
        );
        assert_eq!(
            LauncherState::Error {
                message: String::new(),
                detail: None
            }
            .label(),
            "Error"
        );
    }

    // -- Valid transition tests --

    #[test]
    fn uninitialized_to_extracting() {
        assert!(LauncherState::Uninitialized.can_transition_to(&LauncherState::Extracting {
            message: "starting".into()
        }));
    }

    #[test]
    fn uninitialized_to_prepared() {
        // Skip extraction when pier already exists
        assert!(LauncherState::Uninitialized.can_transition_to(&LauncherState::Prepared));
    }

    #[test]
    fn uninitialized_to_error() {
        assert!(
            LauncherState::Uninitialized.can_transition_to(&LauncherState::Error {
                message: "fail".into(),
                detail: None
            })
        );
    }

    #[test]
    fn extracting_to_prepared() {
        let extracting = LauncherState::Extracting {
            message: "done".into(),
        };
        assert!(extracting.can_transition_to(&LauncherState::Prepared));
    }

    #[test]
    fn extracting_to_error() {
        let extracting = LauncherState::Extracting {
            message: "working".into(),
        };
        assert!(extracting.can_transition_to(&LauncherState::Error {
            message: "fail".into(),
            detail: None
        }));
    }

    #[test]
    fn prepared_to_starting() {
        assert!(LauncherState::Prepared.can_transition_to(&LauncherState::Starting));
    }

    #[test]
    fn prepared_to_uninitialized() {
        // Reset flow
        assert!(LauncherState::Prepared.can_transition_to(&LauncherState::Uninitialized));
    }

    #[test]
    fn starting_to_running() {
        assert!(LauncherState::Starting.can_transition_to(&LauncherState::Running {
            pid: 123,
            started_at: "2026-01-01T00:00:00Z".into()
        }));
    }

    #[test]
    fn starting_to_crashed() {
        assert!(LauncherState::Starting.can_transition_to(&LauncherState::Crashed {
            exit_code: Some(1),
            message: "failed to start".into()
        }));
    }

    #[test]
    fn running_to_stopping() {
        let running = LauncherState::Running {
            pid: 42,
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        assert!(running.can_transition_to(&LauncherState::Stopping));
    }

    #[test]
    fn running_to_crashed() {
        let running = LauncherState::Running {
            pid: 42,
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        assert!(running.can_transition_to(&LauncherState::Crashed {
            exit_code: Some(137),
            message: "killed".into()
        }));
    }

    #[test]
    fn stopping_to_stopped() {
        assert!(LauncherState::Stopping.can_transition_to(&LauncherState::Stopped));
    }

    #[test]
    fn stopping_to_crashed() {
        assert!(LauncherState::Stopping.can_transition_to(&LauncherState::Crashed {
            exit_code: None,
            message: "crash during shutdown".into()
        }));
    }

    #[test]
    fn stopped_to_starting() {
        assert!(LauncherState::Stopped.can_transition_to(&LauncherState::Starting));
    }

    #[test]
    fn stopped_to_uninitialized() {
        // Reset flow
        assert!(LauncherState::Stopped.can_transition_to(&LauncherState::Uninitialized));
    }

    #[test]
    fn crashed_to_starting() {
        let crashed = LauncherState::Crashed {
            exit_code: Some(1),
            message: "oops".into(),
        };
        assert!(crashed.can_transition_to(&LauncherState::Starting));
    }

    #[test]
    fn crashed_to_uninitialized() {
        let crashed = LauncherState::Crashed {
            exit_code: None,
            message: "oops".into(),
        };
        assert!(crashed.can_transition_to(&LauncherState::Uninitialized));
    }

    #[test]
    fn error_to_uninitialized() {
        let error = LauncherState::Error {
            message: "bad".into(),
            detail: None,
        };
        assert!(error.can_transition_to(&LauncherState::Uninitialized));
    }

    #[test]
    fn error_to_extracting() {
        let error = LauncherState::Error {
            message: "bad".into(),
            detail: None,
        };
        assert!(error.can_transition_to(&LauncherState::Extracting {
            message: "retrying".into()
        }));
    }

    #[test]
    fn error_to_prepared() {
        let error = LauncherState::Error {
            message: "bad".into(),
            detail: None,
        };
        assert!(error.can_transition_to(&LauncherState::Prepared));
    }

    #[test]
    fn error_to_starting() {
        let error = LauncherState::Error {
            message: "bad".into(),
            detail: None,
        };
        assert!(error.can_transition_to(&LauncherState::Starting));
    }

    // -- Invalid transition tests --

    #[test]
    fn cannot_jump_uninitialized_to_running() {
        assert!(!LauncherState::Uninitialized.can_transition_to(&LauncherState::Running {
            pid: 1,
            started_at: String::new()
        }));
    }

    #[test]
    fn cannot_jump_uninitialized_to_stopping() {
        assert!(!LauncherState::Uninitialized.can_transition_to(&LauncherState::Stopping));
    }

    #[test]
    fn cannot_jump_extracting_to_running() {
        let extracting = LauncherState::Extracting {
            message: "busy".into(),
        };
        assert!(!extracting.can_transition_to(&LauncherState::Running {
            pid: 1,
            started_at: String::new()
        }));
    }

    #[test]
    fn cannot_jump_stopped_to_running() {
        assert!(!LauncherState::Stopped.can_transition_to(&LauncherState::Running {
            pid: 1,
            started_at: String::new()
        }));
    }

    #[test]
    fn cannot_go_running_to_prepared() {
        let running = LauncherState::Running {
            pid: 1,
            started_at: String::new(),
        };
        assert!(!running.can_transition_to(&LauncherState::Prepared));
    }

    // -- StateMachine tests --

    #[test]
    fn new_state_machine_starts_uninitialized() {
        let sm = StateMachine::new();
        assert_eq!(sm.current(), LauncherState::Uninitialized);
    }

    #[test]
    fn valid_transition_succeeds() {
        let sm = StateMachine::new();
        let result = sm.transition(LauncherState::Extracting {
            message: "starting extraction".into(),
        });
        assert!(result.is_ok());
        assert_eq!(sm.current().label(), "Extracting");
    }

    #[test]
    fn invalid_transition_returns_error() {
        let sm = StateMachine::new();
        let result = sm.transition(LauncherState::Running {
            pid: 1,
            started_at: String::new(),
        });
        assert!(result.is_err());
        match result.unwrap_err() {
            LauncherError::InvalidTransition { from, to } => {
                assert_eq!(from, "Uninitialized");
                assert_eq!(to, "Running");
            }
            other => panic!("expected InvalidTransition, got: {other:?}"),
        }
        // State should not have changed
        assert_eq!(sm.current(), LauncherState::Uninitialized);
    }

    #[test]
    fn full_happy_path_lifecycle() {
        let sm = StateMachine::new();

        sm.transition(LauncherState::Extracting {
            message: "extracting".into(),
        })
        .unwrap();
        sm.transition(LauncherState::Prepared).unwrap();
        sm.transition(LauncherState::Starting).unwrap();
        sm.transition(LauncherState::Running {
            pid: 42,
            started_at: "2026-01-01T00:00:00Z".into(),
        })
        .unwrap();
        sm.transition(LauncherState::Stopping).unwrap();
        sm.transition(LauncherState::Stopped).unwrap();

        assert_eq!(sm.current(), LauncherState::Stopped);
    }

    #[test]
    fn crash_and_restart_lifecycle() {
        let sm = StateMachine::new();

        sm.transition(LauncherState::Extracting {
            message: "extracting".into(),
        })
        .unwrap();
        sm.transition(LauncherState::Prepared).unwrap();
        sm.transition(LauncherState::Starting).unwrap();
        sm.transition(LauncherState::Running {
            pid: 99,
            started_at: "2026-01-01T00:00:00Z".into(),
        })
        .unwrap();
        sm.transition(LauncherState::Crashed {
            exit_code: Some(137),
            message: "killed by signal".into(),
        })
        .unwrap();
        // Restart from crashed
        sm.transition(LauncherState::Starting).unwrap();
        sm.transition(LauncherState::Running {
            pid: 100,
            started_at: "2026-01-01T00:01:00Z".into(),
        })
        .unwrap();

        assert_eq!(sm.current().label(), "Running");
    }

    #[test]
    fn error_recovery_lifecycle() {
        let sm = StateMachine::new();

        sm.transition(LauncherState::Extracting {
            message: "extracting".into(),
        })
        .unwrap();
        sm.transition(LauncherState::Error {
            message: "checksum mismatch".into(),
            detail: Some("expected abc, got def".into()),
        })
        .unwrap();
        // Retry from error
        sm.transition(LauncherState::Extracting {
            message: "retrying".into(),
        })
        .unwrap();
        sm.transition(LauncherState::Prepared).unwrap();

        assert_eq!(sm.current(), LauncherState::Prepared);
    }

    #[test]
    fn reset_from_error() {
        let sm = StateMachine::new();

        sm.transition(LauncherState::Error {
            message: "bad".into(),
            detail: None,
        })
        .unwrap();
        sm.transition(LauncherState::Uninitialized).unwrap();

        assert_eq!(sm.current(), LauncherState::Uninitialized);
    }

    #[test]
    fn force_error_overrides_any_state() {
        let sm = StateMachine::new();

        sm.transition(LauncherState::Extracting {
            message: "working".into(),
        })
        .unwrap();
        sm.transition(LauncherState::Prepared).unwrap();
        sm.transition(LauncherState::Starting).unwrap();

        // Force error from Starting (normally Starting can go to Error via
        // regular transition too, but force_error works from *any* state)
        sm.force_error("fatal panic".into(), Some("stack trace here".into()));

        match sm.current() {
            LauncherState::Error { message, detail } => {
                assert_eq!(message, "fatal panic");
                assert_eq!(detail.unwrap(), "stack trace here");
            }
            other => panic!("expected Error, got: {other:?}"),
        }
    }

    #[test]
    fn state_machine_is_clone_and_shares_state() {
        let sm1 = StateMachine::new();
        let sm2 = sm1.clone();

        sm1.transition(LauncherState::Extracting {
            message: "go".into(),
        })
        .unwrap();

        // sm2 sees the same state because they share the Arc
        assert_eq!(sm2.current().label(), "Extracting");
    }

    #[test]
    fn thread_safety() {
        let sm = StateMachine::new();
        let sm_clone = sm.clone();

        let handle = std::thread::spawn(move || {
            sm_clone
                .transition(LauncherState::Extracting {
                    message: "from thread".into(),
                })
                .unwrap();
        });

        handle.join().unwrap();
        assert_eq!(sm.current().label(), "Extracting");
    }

    // -- Serialization tests --

    #[test]
    fn serializes_uninitialized() {
        let state = LauncherState::Uninitialized;
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(json, serde_json::json!({"state": "Uninitialized"}));
    }

    #[test]
    fn serializes_extracting_with_context() {
        let state = LauncherState::Extracting {
            message: "Verifying checksum...".into(),
        };
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "state": "Extracting",
                "context": {"message": "Verifying checksum..."}
            })
        );
    }

    #[test]
    fn serializes_running_with_context() {
        let state = LauncherState::Running {
            pid: 1234,
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "state": "Running",
                "context": {"pid": 1234, "started_at": "2026-01-01T00:00:00Z"}
            })
        );
    }

    #[test]
    fn serializes_crashed_with_context() {
        let state = LauncherState::Crashed {
            exit_code: Some(137),
            message: "killed by signal".into(),
        };
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "state": "Crashed",
                "context": {"exit_code": 137, "message": "killed by signal"}
            })
        );
    }

    #[test]
    fn serializes_error_with_context() {
        let state = LauncherState::Error {
            message: "checksum mismatch".into(),
            detail: Some("expected abc, got def".into()),
        };
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "state": "Error",
                "context": {
                    "message": "checksum mismatch",
                    "detail": "expected abc, got def"
                }
            })
        );
    }

    #[test]
    fn serializes_error_without_detail() {
        let state = LauncherState::Error {
            message: "something broke".into(),
            detail: None,
        };
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "state": "Error",
                "context": {
                    "message": "something broke",
                    "detail": null
                }
            })
        );
    }

    #[test]
    fn skip_extraction_path() {
        // When pier already exists, go straight to Prepared
        let sm = StateMachine::new();
        sm.transition(LauncherState::Prepared).unwrap();
        sm.transition(LauncherState::Starting).unwrap();
        assert_eq!(sm.current().label(), "Starting");
    }
}
