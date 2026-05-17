//! Output buffer for session replay and persistence

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use uuid::Uuid;

/// A buffered output message with sequence number
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferedOutput {
    pub seq: u64,
    pub content: serde_json::Value,
    pub timestamp: DateTime<Utc>,
}

/// Buffer for storing outputs for replay on session restore
pub struct OutputBuffer {
    session_id: Uuid,
    outputs: VecDeque<BufferedOutput>,
    next_seq: u64,
    max_size: usize,
}

impl OutputBuffer {
    /// Default maximum buffer size
    pub const DEFAULT_MAX_SIZE: usize = 1000;

    /// Create a new empty buffer
    pub fn new(session_id: Uuid) -> Self {
        Self {
            session_id,
            outputs: VecDeque::new(),
            next_seq: 0,
            max_size: Self::DEFAULT_MAX_SIZE,
        }
    }

    /// Create a buffer with custom max size
    pub fn with_max_size(session_id: Uuid, max_size: usize) -> Self {
        Self {
            session_id,
            outputs: VecDeque::new(),
            next_seq: 0,
            max_size,
        }
    }

    /// Get the session ID this buffer belongs to
    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    /// Add output to buffer, returns sequence number
    pub fn push(&mut self, content: serde_json::Value) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;

        self.outputs.push_back(BufferedOutput {
            seq,
            content,
            timestamp: Utc::now(),
        });

        // Enforce max size by removing oldest entries
        while self.outputs.len() > self.max_size {
            self.outputs.pop_front();
        }

        seq
    }

    /// Mark outputs up to (and including) seq as consumed
    pub fn ack(&mut self, seq: u64) {
        while let Some(front) = self.outputs.front() {
            if front.seq <= seq {
                self.outputs.pop_front();
            } else {
                break;
            }
        }
    }

    /// Get all pending (unacked) outputs
    pub fn pending(&self) -> impl Iterator<Item = &BufferedOutput> {
        self.outputs.iter()
    }

    /// Get count of pending outputs
    pub fn pending_count(&self) -> usize {
        self.outputs.len()
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty()
    }

    /// Restore buffer from snapshot data
    pub fn from_snapshot(session_id: Uuid, outputs: Vec<BufferedOutput>) -> Self {
        let next_seq = outputs
            .iter()
            .map(|o| o.seq)
            .max()
            .map(|s| s + 1)
            .unwrap_or(0);
        Self {
            session_id,
            outputs: outputs.into(),
            next_seq,
            max_size: Self::DEFAULT_MAX_SIZE,
        }
    }

    /// Export buffer contents for snapshot
    pub fn to_snapshot(&self) -> Vec<BufferedOutput> {
        self.outputs.iter().cloned().collect()
    }

    /// Clear all buffered outputs
    pub fn clear(&mut self) {
        self.outputs.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_and_ack() {
        let mut buffer = OutputBuffer::new(Uuid::new_v4());

        let seq1 = buffer.push(serde_json::json!({"msg": "first"}));
        let seq2 = buffer.push(serde_json::json!({"msg": "second"}));
        let seq3 = buffer.push(serde_json::json!({"msg": "third"}));

        assert_eq!(buffer.pending_count(), 3);
        assert_eq!(seq1, 0);
        assert_eq!(seq2, 1);
        assert_eq!(seq3, 2);

        buffer.ack(seq1);
        assert_eq!(buffer.pending_count(), 2);

        buffer.ack(seq3);
        assert_eq!(buffer.pending_count(), 0);
    }

    #[test]
    fn test_max_size() {
        let mut buffer = OutputBuffer::with_max_size(Uuid::new_v4(), 3);

        buffer.push(serde_json::json!(1));
        buffer.push(serde_json::json!(2));
        buffer.push(serde_json::json!(3));
        buffer.push(serde_json::json!(4));

        assert_eq!(buffer.pending_count(), 3);

        let seqs: Vec<u64> = buffer.pending().map(|o| o.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3]); // First one was dropped
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let session_id = Uuid::new_v4();
        let mut buffer = OutputBuffer::new(session_id);

        buffer.push(serde_json::json!({"a": 1}));
        buffer.push(serde_json::json!({"b": 2}));

        let snapshot = buffer.to_snapshot();
        let restored = OutputBuffer::from_snapshot(session_id, snapshot);

        assert_eq!(restored.pending_count(), 2);
        assert_eq!(restored.session_id(), session_id);
    }
}
