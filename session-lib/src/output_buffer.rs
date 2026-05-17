//! Output buffer for reliable message delivery with acknowledgments.
//!
//! This module provides a persistent buffer for Claude outputs that ensures
//! no messages are lost during WebSocket disconnects. Messages are held until
//! the backend acknowledges receipt.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use tracing::{debug, warn};
use uuid::Uuid;

/// Maximum number of pending messages to keep in memory before spilling to disk
const MAX_MEMORY_MESSAGES: usize = 1000;

/// A sequenced output message waiting for acknowledgment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingOutput {
    /// Sequence number (monotonically increasing)
    pub seq: u64,
    /// The actual content
    pub content: serde_json::Value,
}

/// Buffer state that can be persisted to disk
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BufferState {
    /// Session ID this buffer belongs to
    session_id: Uuid,
    /// Next sequence number to assign
    next_seq: u64,
    /// Last acknowledged sequence number
    last_ack_seq: u64,
    /// Pending messages (those with seq > last_ack_seq)
    pending: VecDeque<PendingOutput>,
}

/// Pending output buffer with persistence and acknowledgment tracking
pub struct PendingOutputBuffer {
    /// Session ID (kept for logging/debugging)
    #[allow(dead_code)]
    session_id: Uuid,
    /// Path to persistence file
    persist_path: PathBuf,
    /// In-memory buffer state
    state: BufferState,
    /// Whether we have unsaved changes
    dirty: bool,
}

impl PendingOutputBuffer {
    /// Create or load a buffer for the given session
    pub fn new(session_id: Uuid) -> Result<Self> {
        let persist_path = Self::buffer_path(session_id)?;

        // Try to load existing state
        let state = if persist_path.exists() {
            match fs::read_to_string(&persist_path) {
                Ok(contents) => match serde_json::from_str::<BufferState>(&contents) {
                    Ok(mut state) => {
                        // Verify session ID matches
                        if state.session_id != session_id {
                            warn!(
                                "Buffer file session ID mismatch, creating fresh buffer. File: {}, Expected: {}",
                                state.session_id, session_id
                            );
                            BufferState {
                                session_id,
                                ..Default::default()
                            }
                        } else {
                            debug!(
                                "Loaded pending buffer: {} messages, next_seq={}, last_ack={}",
                                state.pending.len(),
                                state.next_seq,
                                state.last_ack_seq
                            );
                            // Filter out any already-acked messages (safety check)
                            state.pending.retain(|msg| msg.seq > state.last_ack_seq);
                            state
                        }
                    }
                    Err(e) => {
                        warn!("Failed to parse buffer file, creating fresh: {}", e);
                        BufferState {
                            session_id,
                            ..Default::default()
                        }
                    }
                },
                Err(e) => {
                    warn!("Failed to read buffer file, creating fresh: {}", e);
                    BufferState {
                        session_id,
                        ..Default::default()
                    }
                }
            }
        } else {
            BufferState {
                session_id,
                ..Default::default()
            }
        };

        Ok(Self {
            session_id,
            persist_path,
            state,
            dirty: false,
        })
    }

    /// Get the path for a session's buffer file
    fn buffer_path(session_id: Uuid) -> Result<PathBuf> {
        let config_dir = directories::ProjectDirs::from("com", "anthropic", "agent-portal")
            .context("Failed to determine config directory")?
            .config_dir()
            .to_path_buf();

        // Create buffers subdirectory
        let buffers_dir = config_dir.join("buffers");
        fs::create_dir_all(&buffers_dir).context("Failed to create buffers directory")?;

        Ok(buffers_dir.join(format!("{}.json", session_id)))
    }

    /// Add a new output to the buffer, returning the assigned sequence number
    pub fn push(&mut self, content: serde_json::Value) -> u64 {
        let seq = self.state.next_seq;
        self.state.next_seq += 1;

        self.state.pending.push_back(PendingOutput { seq, content });

        self.dirty = true;

        // Trim if too many messages in memory (keep the most recent ones)
        if self.state.pending.len() > MAX_MEMORY_MESSAGES {
            // Keep the last MAX_MEMORY_MESSAGES
            while self.state.pending.len() > MAX_MEMORY_MESSAGES {
                if let Some(removed) = self.state.pending.pop_front() {
                    warn!(
                        "Buffer overflow, dropping oldest message seq={}",
                        removed.seq
                    );
                }
            }
        }

        debug!(
            "Buffered output seq={}, pending={}",
            seq,
            self.state.pending.len()
        );
        seq
    }

    /// Acknowledge receipt of all messages up to and including the given sequence
    pub fn acknowledge(&mut self, ack_seq: u64) {
        if ack_seq <= self.state.last_ack_seq {
            debug!(
                "Ignoring duplicate/old ack: {} <= {}",
                ack_seq, self.state.last_ack_seq
            );
            return;
        }

        let before = self.state.pending.len();
        self.state.pending.retain(|msg| msg.seq > ack_seq);
        let after = self.state.pending.len();

        self.state.last_ack_seq = ack_seq;
        self.dirty = true;

        debug!(
            "Acknowledged up to seq={}, removed {} messages, {} remaining",
            ack_seq,
            before - after,
            after
        );
    }

    /// Get all pending (unacknowledged) messages for replay
    pub fn get_pending(&self) -> impl Iterator<Item = &PendingOutput> {
        self.state.pending.iter()
    }

    /// Get the number of pending messages
    pub fn pending_count(&self) -> usize {
        self.state.pending.len()
    }

    /// Drop all pending messages (used when replay fails repeatedly)
    pub fn clear(&mut self) {
        self.state.pending.clear();
        self.dirty = true;
    }

    /// Get the last acknowledged sequence number
    pub fn last_ack_seq(&self) -> u64 {
        self.state.last_ack_seq
    }

    /// Persist the buffer state to disk
    pub fn persist(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }

        let contents = serde_json::to_string_pretty(&self.state)
            .context("Failed to serialize buffer state")?;

        // Write to temp file first for atomicity
        let temp_path = self.persist_path.with_extension("tmp");
        fs::write(&temp_path, &contents).context("Failed to write temp buffer file")?;

        // Atomic rename
        fs::rename(&temp_path, &self.persist_path).context("Failed to rename buffer file")?;

        self.dirty = false;
        debug!(
            "Persisted buffer state: {} pending messages",
            self.state.pending.len()
        );

        Ok(())
    }
}

impl Drop for PendingOutputBuffer {
    fn drop(&mut self) {
        // Best-effort persist on drop
        if self.dirty {
            if let Err(e) = self.persist() {
                warn!("Failed to persist buffer on drop: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_and_acknowledge() {
        let session_id = Uuid::new_v4();
        let mut buffer = PendingOutputBuffer {
            session_id,
            persist_path: PathBuf::from("/tmp/test_buffer.json"),
            state: BufferState {
                session_id,
                ..Default::default()
            },
            dirty: false,
        };

        // Push some messages
        let seq1 = buffer.push(serde_json::json!({"type": "test", "n": 1}));
        let seq2 = buffer.push(serde_json::json!({"type": "test", "n": 2}));
        let seq3 = buffer.push(serde_json::json!({"type": "test", "n": 3}));

        assert_eq!(seq1, 0);
        assert_eq!(seq2, 1);
        assert_eq!(seq3, 2);
        assert_eq!(buffer.pending_count(), 3);

        // Acknowledge first two
        buffer.acknowledge(1);
        assert_eq!(buffer.pending_count(), 1);
        assert_eq!(buffer.last_ack_seq(), 1);

        // Remaining message should be seq=2
        let pending: Vec<_> = buffer.get_pending().collect();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].seq, 2);
    }

    #[test]
    fn test_duplicate_acknowledge() {
        let session_id = Uuid::new_v4();
        let mut buffer = PendingOutputBuffer {
            session_id,
            persist_path: PathBuf::from("/tmp/test_buffer2.json"),
            state: BufferState {
                session_id,
                ..Default::default()
            },
            dirty: false,
        };

        // Push 3 messages: seq 0, 1, 2
        buffer.push(serde_json::json!({"n": 1}));
        buffer.push(serde_json::json!({"n": 2}));
        buffer.push(serde_json::json!({"n": 3}));
        assert_eq!(buffer.pending_count(), 3);

        // Acknowledge up to seq 1 (removes seq 0 and 1, keeps seq 2)
        buffer.acknowledge(1);
        assert_eq!(buffer.pending_count(), 1);

        // Duplicate ack should be ignored (no change)
        buffer.acknowledge(1);
        assert_eq!(buffer.pending_count(), 1);

        // Old ack should be ignored (no change)
        buffer.acknowledge(0);
        assert_eq!(buffer.pending_count(), 1);

        // Verify the remaining message is seq=2
        let pending: Vec<_> = buffer.get_pending().collect();
        assert_eq!(pending[0].seq, 2);
    }

    #[test]
    fn test_overflow_protection() {
        let session_id = Uuid::new_v4();
        let mut buffer = PendingOutputBuffer {
            session_id,
            persist_path: PathBuf::from("/tmp/test_buffer3.json"),
            state: BufferState {
                session_id,
                ..Default::default()
            },
            dirty: false,
        };

        // Push more than MAX_MEMORY_MESSAGES
        for i in 0..MAX_MEMORY_MESSAGES + 100 {
            buffer.push(serde_json::json!({"n": i}));
        }

        // Should be capped at MAX_MEMORY_MESSAGES
        assert_eq!(buffer.pending_count(), MAX_MEMORY_MESSAGES);

        // The oldest messages should have been dropped
        let first = buffer.get_pending().next().unwrap();
        assert_eq!(first.seq, 100); // First 100 were dropped
    }
}
