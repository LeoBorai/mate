/// Generates the nth Fibonacci number (0-indexed) as an `f64`.
///
/// Returns `None` if `n` is negative or if the result exceeds safe integer representation
/// or floating point limits.
pub fn fibonacci(n: u32) -> Option<f64> {
    match n {
        0 => Some(0.0),
        1 => Some(1.0),
        _ => {
            let mut a = 0.0f64;
            let mut b = 1.0f64;
            for _ in 2..=n {
                let next = a + b;
                if next.is_infinite() {
                    return None;
                }
                a = b;
                b = next;
            }
            Some(b)
        }
    }
}

/// Generates a vector containing the Fibonacci sequence up to `n` numbers using `f64`.
pub fn fibonacci_sequence(n: usize) -> Vec<f64> {
    let mut seq = Vec::with_capacity(n);
    if n == 0 {
        return seq;
    }
    
    let mut a = 0.0f64;
    let mut b = 1.0f64;
    
    for i in 0..n {
        match i {
            0 => seq.push(0.0),
            1 => seq.push(1.0),
            _ => {
                let next = a + b;
                a = b;
                b = next;
                seq.push(b);
            }
        }
    }
    seq
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fibonacci() {
        assert_eq!(fibonacci(0), Some(0.0), "fibonacci(0) must be 0.0");
        assert_eq!(fibonacci(1), Some(1.0), "fibonacci(1) must be 1.0");
        assert_eq!(fibonacci(2), Some(1.0), "fibonacci(2) must be 1.0");
        assert_eq!(fibonacci(3), Some(2.0), "fibonacci(3) must be 2.0");
        assert_eq!(fibonacci(10), Some(55.0), "fibonacci(10) must be 55.0");
    }

    #[test]
    fn test_fibonacci_sequence() {
        assert_eq!(fibonacci_sequence(0), vec![], "empty sequence for n=0");
        assert_eq!(fibonacci_sequence(1), vec![0.0], "sequence of length 1");
        assert_eq!(fibonacci_sequence(5), vec![0.0, 1.0, 1.0, 2.0, 3.0], "sequence of length 5");
    }
}
