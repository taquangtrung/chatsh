use std::collections::VecDeque;

const DEFAULT_BUFFER_CAPACITY: usize = 200;

pub struct ContextEntry {
    pub line: String,
}

pub struct ContextBuffer {
    entries: VecDeque<ContextEntry>,
    capacity: usize,
}

impl Default for ContextBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextBuffer {
    pub fn new() -> Self {
        Self {
            entries: VecDeque::with_capacity(DEFAULT_BUFFER_CAPACITY),
            capacity: DEFAULT_BUFFER_CAPACITY,
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, line: String) {
        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(ContextEntry { line });
    }

    pub fn recent_lines(&self, count: usize) -> Vec<&ContextEntry> {
        let start = self.entries.len().saturating_sub(count);
        self.entries.range(start..).collect()
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_within_capacity() {
        let mut buf = ContextBuffer::with_capacity(3);
        buf.push("a".to_string());
        buf.push("b".to_string());
        buf.push("c".to_string());
        assert_eq!(buf.recent_lines(10).len(), 3);
    }

    #[test]
    fn test_push_evicts_oldest() {
        let mut buf = ContextBuffer::with_capacity(2);
        buf.push("a".to_string());
        buf.push("b".to_string());
        buf.push("c".to_string());
        let lines = buf.recent_lines(10);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].line, "b");
        assert_eq!(lines[1].line, "c");
    }

    #[test]
    fn test_recent_lines_limits_count() {
        let mut buf = ContextBuffer::with_capacity(10);
        for i in 0..10 {
            buf.push(format!("{i}"));
        }
        let lines = buf.recent_lines(3);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].line, "7");
        assert_eq!(lines[2].line, "9");
    }
}
