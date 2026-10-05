use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct Ring {
    lines: VecDeque<String>,
    bytes: usize,
    limit: usize,
}

impl Ring {
    pub fn new(limit: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            bytes: 0,
            limit,
        }
    }

    pub fn push(&mut self, line: impl Into<String>) {
        let line = line.into();
        self.bytes += line.len();
        self.lines.push_back(line);

        while self.bytes > self.limit {
            let Some(old) = self.lines.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(old.len());
        }
    }

    pub fn tail(&self, count: usize) -> Vec<String> {
        self.lines
            .iter()
            .rev()
            .take(count)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }
}

impl Default for Ring {
    fn default() -> Self {
        Self::new(64 * 1024)
    }
}
