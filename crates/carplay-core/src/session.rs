// SPDX-License-Identifier: GPL-3.0-only
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Idle,
    Preflight,
    Bootstrap,
    Authenticating,
    Negotiating,
    Streaming,
    Reconnecting,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    PrerequisitesReady,
    TransportReady,
    Authenticated,
    FirstFrame,
    Disconnected,
    Failed(String),
}

/// Generation tokens prevent callbacks from a previous connection from mutating a new session.
#[derive(Debug)]
pub struct Session {
    generation: u64,
    phase: Phase,
    attempt: u32,
    reason: Option<String>,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            generation: 0,
            phase: Phase::Idle,
            attempt: 0,
            reason: None,
        }
    }
}

impl Session {
    pub fn start(&mut self) -> u64 {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("generation exhausted");
        self.phase = Phase::Preflight;
        self.reason = None;
        self.generation
    }
    pub fn stop(&mut self) {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("generation exhausted");
        self.phase = Phase::Idle;
        self.attempt = 0;
        self.reason = None;
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
    pub fn reconnect_delay_ms(&self) -> u64 {
        (500u64.saturating_mul(1u64 << self.attempt.min(6))).min(30_000)
    }
    pub fn apply(&mut self, generation: u64, event: Event) -> bool {
        if generation != self.generation || self.phase == Phase::Idle {
            return false;
        }
        self.phase = match (&self.phase, event) {
            (Phase::Preflight, Event::PrerequisitesReady) => Phase::Bootstrap,
            (Phase::Bootstrap, Event::TransportReady) => Phase::Authenticating,
            (Phase::Authenticating, Event::Authenticated) => Phase::Negotiating,
            (Phase::Negotiating, Event::FirstFrame) => {
                self.attempt = 0;
                Phase::Streaming
            }
            (_, Event::Failed(reason)) => {
                self.reason = Some(reason);
                Phase::Failed
            }
            (
                Phase::Bootstrap | Phase::Authenticating | Phase::Negotiating | Phase::Streaming,
                Event::Disconnected,
            ) => {
                self.attempt = self.attempt.saturating_add(1);
                Phase::Reconnecting
            }
            _ => return false,
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_frame_cannot_connect_new_session() {
        let mut s = Session::default();
        let old = s.start();
        let current = s.start();
        assert!(!s.apply(old, Event::FirstFrame));
        assert!(!s.apply(current, Event::FirstFrame));
        for e in [
            Event::PrerequisitesReady,
            Event::TransportReady,
            Event::Authenticated,
            Event::FirstFrame,
        ] {
            assert!(s.apply(current, e));
        }
        assert_eq!(s.phase(), Phase::Streaming);
        s.stop();
        assert!(!s.apply(current, Event::Disconnected));
        assert_eq!(s.phase(), Phase::Idle);
    }
}
