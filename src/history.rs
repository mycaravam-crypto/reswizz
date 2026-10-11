use std::collections::VecDeque;

/// Fixed-capacity time series; oldest sample is dropped when full.
pub struct History {
    pub cap: usize,
    pub samples: VecDeque<(f64, f64)>, // (seconds since start, value)
}

impl History {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            samples: VecDeque::with_capacity(cap),
        }
    }

    pub fn push(&mut self, t: f64, v: f64) {
        if self.samples.len() == self.cap {
            self.samples.pop_front();
        }
        self.samples.push_back((t, v));
    }

    pub fn points(&self) -> Vec<[f64; 2]> {
        self.samples.iter().map(|&(t, v)| [t, v]).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_and_ordered() {
        let mut h = History::new(3);
        for i in 0..10 {
            h.push(i as f64, i as f64 * 2.0);
        }
        assert_eq!(h.samples.len(), 3);
        assert_eq!(h.points(), vec![[7.0, 14.0], [8.0, 16.0], [9.0, 18.0]]);
    }
}
