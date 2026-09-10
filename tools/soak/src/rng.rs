//! A small, seedable generator. The workload must be reproducible from
//! its seed on every OS and every build, which rules out anything whose
//! stream is not part of its contract. xorshift64* is enough: the soak
//! needs spread, not cryptographic quality.

#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        // A zero state would stay zero forever.
        Self {
            state: seed ^ 0x9E37_79B9_7F4A_7C15 | 1,
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..bound` (`bound` must be non-zero).
    pub fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }

    pub fn below_usize(&mut self, bound: usize) -> usize {
        self.below(bound as u64) as usize
    }

    /// `true` with probability `percent / 100`.
    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    /// Picks one element of a non-empty slice.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below_usize(items.len())]
    }

    /// A short lowercase name, unique enough inside one run.
    pub fn name(&mut self, prefix: &str) -> String {
        format!("{prefix}-{:06x}", self.below(0x100_0000))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_yields_the_same_stream() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let mut c = Rng::new(8);
        assert_ne!(Rng::new(7).next_u64(), c.next_u64());
    }

    #[test]
    fn below_stays_inside_its_bound() {
        let mut rng = Rng::new(3);
        for _ in 0..10_000 {
            assert!(rng.below(13) < 13);
        }
    }
}
