use std::collections::BTreeSet;

pub(crate) fn step_index(current: usize, delta: i32, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        (current as i32 + delta).clamp(0, len as i32 - 1) as usize
    }
}

pub(crate) fn wrap_index(current: usize, delta: i32, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        (current as i32 + delta).rem_euclid(len as i32) as usize
    }
}

pub(crate) fn clamp_index(current: usize, len: usize) -> usize {
    if len == 0 { 0 } else { current.min(len - 1) }
}

pub(crate) fn range_set(a: usize, b: usize) -> BTreeSet<usize> {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    (lo..=hi).collect()
}

pub(crate) fn xorshift_shuffle<T>(items: &mut [T], mut state: u64) {
    for index in (1..items.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        items.swap(index, state as usize % (index + 1));
    }
}

pub(crate) fn counted(n: usize, singular: &str) -> String {
    if n == 1 {
        format!("1 {singular}")
    } else {
        format!("{n} {singular}s")
    }
}

pub(crate) fn on_off(value: bool) -> String {
    if value { "On".into() } else { "Off".into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_index_clamps_and_handles_empty() {
        assert_eq!(step_index(0, -1, 0), 0);
        assert_eq!(step_index(2, 5, 4), 3);
        assert_eq!(step_index(2, -2, 4), 0);
        assert_eq!(step_index(1, 1, 3), 2);
    }

    #[test]
    fn wrap_index_wraps_and_handles_empty() {
        assert_eq!(wrap_index(0, -1, 0), 0);
        assert_eq!(wrap_index(0, -1, 3), 2);
        assert_eq!(wrap_index(2, 1, 3), 0);
        assert_eq!(wrap_index(1, 2, 4), 3);
    }

    #[test]
    fn xorshift_shuffle_is_a_stable_permutation() {
        let mut first = [0, 1, 2, 3, 4];
        xorshift_shuffle(&mut first, 42);
        let mut second = [0, 1, 2, 3, 4];
        xorshift_shuffle(&mut second, 42);
        assert_eq!(first, second);
        let mut sorted = first;
        sorted.sort_unstable();
        assert_eq!(sorted, [0, 1, 2, 3, 4]);
    }
}
