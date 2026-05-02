use std::collections::VecDeque;
use std::fmt;

pub const DEFAULT_REPLAY_CAPACITY: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayBuffer {
    capacity: usize,
    bytes: VecDeque<u8>,
}

impl ReplayBuffer {
    pub fn new(capacity: usize) -> Result<Self, ReplayBufferError> {
        if capacity == 0 {
            return Err(ReplayBufferError::ZeroCapacity);
        }

        Ok(Self {
            capacity,
            bytes: VecDeque::with_capacity(capacity),
        })
    }

    pub fn default_capacity() -> Self {
        Self::new(DEFAULT_REPLAY_CAPACITY).expect("default replay capacity is nonzero")
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn append(&mut self, bytes: &[u8]) {
        if bytes.len() >= self.capacity {
            self.bytes.clear();
            self.bytes
                .extend(bytes[bytes.len() - self.capacity..].iter().copied());
            return;
        }

        let overflow = self
            .bytes
            .len()
            .saturating_add(bytes.len())
            .saturating_sub(self.capacity);
        self.bytes.drain(..overflow);
        self.bytes.extend(bytes.iter().copied());
    }

    pub fn snapshot(&self) -> Vec<u8> {
        self.bytes.iter().copied().collect()
    }

    pub fn clear(&mut self) {
        self.bytes.clear();
    }
}

impl Default for ReplayBuffer {
    fn default() -> Self {
        Self::default_capacity()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayBufferError {
    ZeroCapacity,
}

impl fmt::Display for ReplayBufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroCapacity => write!(f, "replay buffer capacity must be greater than zero"),
        }
    }
}

impl std::error::Error for ReplayBufferError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_buffer_keeps_recent_bytes() {
        let mut buffer = ReplayBuffer::new(5).unwrap();
        buffer.append(b"abc");
        buffer.append(b"def");

        assert_eq!(buffer.snapshot(), b"bcdef");
    }

    #[test]
    fn append_larger_than_capacity_keeps_tail() {
        let mut buffer = ReplayBuffer::new(4).unwrap();
        buffer.append(b"abcdefghijkl");

        assert_eq!(buffer.snapshot(), b"ijkl");
    }

    #[test]
    fn zero_capacity_is_rejected() {
        assert_eq!(
            ReplayBuffer::new(0).unwrap_err(),
            ReplayBufferError::ZeroCapacity
        );
    }
}
